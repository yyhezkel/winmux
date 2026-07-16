import { Terminal, type IDisposable, type ILink, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { ClipboardAddon } from "@xterm/addon-clipboard";
import type {
  IClipboardProvider,
  ClipboardSelectionType,
} from "@xterm/addon-clipboard";

// Phase LL: OSC 52 clipboard provider — WRITE-ONLY. A remote program (e.g.
// Claude's fullscreen renderer) can copy its selection into the OS clipboard
// via OSC 52; but we deliberately return "" for OSC 52 READ queries so a
// remote can NEVER exfiltrate the user's local clipboard. The addon hands us
// the already-base64-decoded text.
const g_oscClipboardProvider: IClipboardProvider = {
  readText(_selection: ClipboardSelectionType): string {
    return "";
  },
  async writeText(
    _selection: ClipboardSelectionType,
    text: string,
  ): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      console.debug("[osc52] copied", text.length, "chars to clipboard");
    } catch (e) {
      console.warn("OSC52 clipboard write failed", e);
    }
  },
};
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { reorderRtlForDisplay } from "./bidi";
import { detectDirection, detectRowDirections } from "./textDirection";
import { transformMouseX, findRow } from "./mouseRtl";
import { t } from "./i18n";

// Phase 62.B (item J): parse a `file://` URI (as emitted in Claude Code's
// OSC 8 hyperlinks, e.g. `file:///home/runner/.env.prod`) into the bare
// remote path. Returns null for non-file URIs. Handles the empty-host
// form (`file:///path`) and a `file://host/path` form, plus percent-
// decoding.
function fileUriToPath(uri: string): string | null {
  if (!uri.startsWith("file://")) return null;
  let rest = uri.slice("file://".length);
  if (!rest.startsWith("/")) {
    // `file://host/path` — drop the host component.
    const slash = rest.indexOf("/");
    rest = slash >= 0 ? rest.slice(slash) : "/";
  }
  try {
    rest = decodeURIComponent(rest);
  } catch {
    // Leave as-is if it isn't valid percent-encoding.
  }
  return rest;
}

// Phase 64 (J, Track B): Claude Code prints produced files as PLAIN TEXT —
// `[file] <path> (<size>)` — with no OSC 8 wrapping (confirmed live
// 2026-07-15: the OSC 8 transport works for Claude's *other* links, but
// `[file]` never arrives as one). This regex drives a custom xterm link
// provider that makes the `[file] <path> (<size>)` run clickable. The path
// is the first whitespace-free token after the tag; the trailing size paren
// is optional so the match survives format drift.
const FILE_LINK_RE = /\[file\]\s+(\S+)(?:\s+\(([^()]{1,32})\))?/g;

// Phase 9.A live font apply + Phase 15.A per-line auto-direction.
//
// A global registry of all live terminals lets a settings change walk
// every open pane and re-apply the new font / RTL mode. The current
// values are also tracked so a freshly constructed terminal picks up
// the user's choice rather than the hard-coded defaults below.
//
// `rtl_mode` selects how mixed Hebrew / Arabic text is displayed:
//   - "auto_per_line" (default in 15.A): no logical-order reorder; the
//     terminal uses the DOM renderer and each row div gets `dir="auto"`
//     so the browser decides direction per line — first strong
//     directional character wins, exactly like Termius. Mirrors what
//     most users expect for an SSH prompt that prints Hebrew.
//   - "bidi_reorder" (legacy v1 behavior): WebGL renderer + bidi-js
//     reorder bytes into visual order before writing. Faster, but
//     surprises users who expect editable lines to remain in logical
//     order, and breaks on selection / copy.
//   - "off": WebGL renderer, no reorder. Hebrew prints
//     logical-order-as-written, which most monospace fonts show
//     left-to-right.
//
// 1pt ≈ 1.333px at 96dpi; xterm.js wants pixels.
const PT_TO_PX = 1.3333;
export type RtlMode = "auto_per_line" | "bidi_reorder" | "off";

// Minimal typed view into xterm's private CharSizeService so we can call its
// own measure() directly and read the measured cell — without an `any`.
interface XtermInternals {
  _core?: {
    _charSizeService?: {
      width?: number;
      height?: number;
      hasValidSize?: boolean;
      measure?: () => void;
    };
  };
}

let g_fontFamily: string | null = null;
let g_fontSizePx: number | null = null;
/** Redesign pass 4: the terminal follows the active theme. Cached so new
 *  panes construct with it; null until the first applyTheme() pushes one
 *  (the constructor then falls back to the Tokyo Night default below). */
let g_termTheme: ITheme | null = null;
let g_rtlMode: RtlMode = "auto_per_line";
/** Phase 16: when true, Ctrl+C with a non-empty selection copies the
 *  selection to clipboard instead of sending SIGINT. Mirrors Windows
 *  Terminal / VS Code's behavior. Settings → Shortcuts toggles it. */
let g_ctrlCCopyOnSelect = true;
/** Phase HH: mirror Left/Right arrows on RTL lines (default on; only
 *  active when the cursor's line is actually RTL). Settings → Terminal. */
let g_mirrorArrowsRtl = true;
/** Phase 65.O (round 6): one-time guard so the "no wheel proxy" note is
 *  logged once, not once per pane. */
let g_loggedNoWheelProxy = false;
const g_terminals: Set<TerminalInstance> = new Set();

// v0.4.4-beta.2: mouse-tracking leak recovery. When a full-screen app
// (vim `mouse=a`, fzf, less, htop) enables SGR/X10 mouse tracking and then
// exits UNCLEANLY (Ctrl+C, SSH drop, kill), it never sends the disable
// sequence, so xterm.js keeps mouse reporting on — every later click in the
// bare shell sends `\e[<0;x;yM` (SGR 1006) which the shell prints as literal
// text. Writing these DECRST sequences to xterm (NOT the PTY) clears xterm's
// mouse state so it stops emitting mouse events. Covers X10 (1000),
// button-event (1002), any-event (1003), SGR (1006), urxvt (1015), and X10
// hilite (9). Rule #1: this is a fixed control string, never PTY content.
const MOUSE_DISABLE_SEQ =
  "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?9l";
/** v0.4.4-beta.2: when true (default) each pane clears stale mouse-tracking
 *  state on connect/attach. Settings → Terminal. */
let g_autoResetOnConnect = true;
export function setAutoResetOnConnect(on: boolean): void {
  g_autoResetOnConnect = on;
}

/**
 * Push a new font family + size into every live xterm and remember the
 * values so future TerminalInstance constructions inherit them. Family
 * is passed through `quoteFamily()`-style fallbacks already by the
 * caller. Called from App.tsx on settings load and on every
 * `settings:changed`.
 */
/** Tokyo Night — the historical hardcoded palette, kept as the fallback
 *  for terminals constructed before the first applyTheme() call. */
const DEFAULT_TERM_THEME: ITheme = {
  background: "#0e1116",
  foreground: "#e6edf3",
  cursor: "#7aa2f7",
  cursorAccent: "#0e1116",
  selectionBackground: "rgba(122, 162, 247, 0.35)",
  black: "#15161e",
  red: "#f7768e",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#a9b1d6",
  brightBlack: "#414868",
  brightRed: "#ff7a93",
  brightGreen: "#b9f27c",
  brightYellow: "#ff9e64",
  brightBlue: "#7da6ff",
  brightMagenta: "#bb9af7",
  brightCyan: "#0db9d7",
  brightWhite: "#c0caf5",
};

/**
 * Redesign pass 4: push the active theme's terminal palette into every
 * live xterm and cache it for future constructions — same contract as
 * setTerminalFont. Called from applyTheme() on load, preset switch, and
 * any base-colour edit.
 */
export function setTerminalTheme(theme: ITheme): void {
  g_termTheme = theme;
  for (const ti of g_terminals) {
    try {
      ti.term.options.theme = theme;
      // Keep the contrast floor on terminals constructed before this
      // module version (HMR) — setting it repeatedly is idempotent.
      ti.term.options.minimumContrastRatio = 4.5;
      ti.term.refresh(0, ti.term.rows - 1);
    } catch (e) {
      console.warn("setTerminalTheme: per-instance update failed", e);
    }
  }
}

export function setTerminalFont(family: string, sizePt: number): void {
  const px = Math.max(8, Math.round(sizePt * PT_TO_PX));
  g_fontFamily = family;
  g_fontSizePx = px;
  for (const ti of g_terminals) {
    try {
      ti.logFontSwap("before");
      ti.term.options.fontFamily = family;
      ti.term.options.fontSize = px;
      ti.fitAndResize();
      ti.term.refresh(0, ti.term.rows - 1);
      ti.logFontSwap("afterSet");
      requestAnimationFrame(() => ti.logFontSwap("settled"));
    } catch (e) {
      console.warn("setTerminalFont: per-instance update failed", e);
    }
  }
}

/**
 * Change ONLY the terminal font size (keeps the current family). Used by the
 * pop-out window's Ctrl+wheel zoom — a separate webview context, so this only
 * touches that context's terminals, never the main grid. `sizePt` is in
 * points; converted to px with the same clamp as setTerminalFont.
 */
export function setTerminalFontSize(sizePt: number): void {
  const px = Math.max(8, Math.round(sizePt * PT_TO_PX));
  g_fontSizePx = px;
  for (const ti of g_terminals) {
    try {
      ti.term.options.fontSize = px;
      ti.fitAndResize();
      ti.term.refresh(0, ti.term.rows - 1);
    } catch (e) {
      console.warn("setTerminalFontSize: per-instance update failed", e);
    }
  }
}

/**
 * Phase 15.A: switch the RTL handling strategy. Existing terminals
 * keep their previously-constructed renderer (DOM vs WebGL changes
 * require a fresh xterm), so this only affects newly-opened panes —
 * we surface a hint in the Settings UI so the user knows to re-open
 * affected panes. The reorder pipeline (writeData) flips immediately
 * for all current panes.
 */
export function setRtlMode(mode: RtlMode): void {
  g_rtlMode = mode;
  // For panes already in auto-per-line mode, ensure the dir="auto"
  // observer is running. For panes constructed before the switch, this
  // is a no-op if their renderer doesn't match — they'll pick the new
  // strategy on next construction.
  for (const ti of g_terminals) {
    ti.ensureDirObserver();
  }
}

export function getRtlMode(): RtlMode {
  return g_rtlMode;
}

// v0.4.4 (RTL Approach C): the per-line auto-direction escape hatch. When ON
// (default), each terminal row in `auto_per_line` mode gets an explicit
// `dir` computed by detectDirection (mixed→RTL, pure-Latin→LTR). When OFF,
// rows fall back to plain `dir="ltr"` (classic terminal, no BiDi flipping).
// Only meaningful in `auto_per_line` mode — the other RtlMode paths ignore it.
let g_autoDirection = true;
export function setAutoDirection(on: boolean): void {
  if (g_autoDirection === on) return;
  g_autoDirection = on;
  // Re-run the direction pass on every live terminal so the change is live.
  for (const ti of g_terminals) ti.applyRowDirections(true);
}
export function getAutoDirection(): boolean {
  return g_autoDirection;
}

/** Phase 16: flip the Ctrl+C-copies-on-selection behavior at runtime. */
export function setCtrlCCopyOnSelect(enabled: boolean): void {
  g_ctrlCCopyOnSelect = enabled;
}

/** Phase HH: flip RTL arrow-key mirroring at runtime. */
export function setMirrorArrowsRtl(enabled: boolean): void {
  g_mirrorArrowsRtl = enabled;
}


/** Phase HH: swap a Left/Right cursor-key escape sequence to the other
 *  direction. Handles both normal (`\e[C`/`\e[D`) and application-cursor
 *  mode (`\eOC`/`\eOD`), so it's correct regardless of the TUI's mode.
 *  Returns the input unchanged if it isn't a horizontal arrow. */
function swapArrowSeq(data: string): string {
  switch (data) {
    case "\x1b[C":
      return "\x1b[D";
    case "\x1b[D":
      return "\x1b[C";
    case "\x1bOC":
      return "\x1bOD";
    case "\x1bOD":
      return "\x1bOC";
    default:
      return data;
  }
}

/** Paste arbitrary text into the active terminal. xterm.js will wrap
 *  the bytes with bracketed-paste escape codes if the connected shell
 *  has enabled the mode (which most modern shells do). Falls back to
 *  the first focused terminal if a specific instance isn't passed. */
export function pasteIntoActiveTerminal(text: string): void {
  if (!text) return;
  let target: TerminalInstance | null = null;
  for (const ti of g_terminals) {
    if (ti.container.contains(document.activeElement)) {
      target = ti;
      break;
    }
  }
  if (!target) target = g_terminals.values().next().value ?? null;
  try {
    target?.term.paste(text);
    // Phase 65 (bug X): keep focus in the terminal after a paste.
    // Reading the clipboard (and the menu/keystroke that triggered it)
    // can pull focus off the xterm textarea, so the caret "jumps" out of
    // the pane. Re-assert focus on the pasted-into terminal.
    target?.term.focus();
  } catch (e) {
    console.warn("paste failed", e);
  }
}

/** Copy the current xterm.js selection (if any) to the system
 *  clipboard. Returns true on success — the caller uses the boolean
 *  to decide whether to fall through to a different binding. */
export async function copyTerminalSelection(): Promise<boolean> {
  for (const ti of g_terminals) {
    if (!ti.container.contains(document.activeElement)) continue;
    const sel = ti.term.getSelection();
    if (!sel) return false;
    try {
      await navigator.clipboard.writeText(sel);
      return true;
    } catch (e) {
      console.warn("clipboard.writeText failed", e);
      return false;
    }
  }
  return false;
}

// Phase 62.A (item E): custom terminal right-click menu. Phase 60
// blocked the native WebView2 context menu (which the user had been
// using to Copy/Paste in the PLAIN terminal) and replaced right-click
// with paste-only — that read as "copy AND paste stopped working".
// This restores a discoverable Copy / Paste / Select-all menu. The
// native menu stays suppressed. One menu at a time, document-wide.
let g_termMenu: HTMLDivElement | null = null;
function dismissTerminalMenu(): void {
  if (!g_termMenu) return;
  g_termMenu.remove();
  g_termMenu = null;
  document.removeEventListener("mousedown", onTermMenuOutside, true);
  document.removeEventListener("keydown", onTermMenuKey, true);
  window.removeEventListener("blur", dismissTerminalMenu);
  window.removeEventListener("resize", dismissTerminalMenu);
}
function onTermMenuOutside(e: MouseEvent): void {
  if (g_termMenu && !g_termMenu.contains(e.target as Node)) dismissTerminalMenu();
}
function onTermMenuKey(e: KeyboardEvent): void {
  if (e.key === "Escape") dismissTerminalMenu();
}
function showTerminalContextMenu(ti: TerminalInstance, x: number, y: number): void {
  dismissTerminalMenu();
  const sel = ti.term.getSelection();
  const menu = document.createElement("div");
  menu.className = "term-ctx-menu";
  const addItem = (label: string, enabled: boolean, action: () => void) => {
    const b = document.createElement("button");
    b.className = "term-ctx-item";
    b.textContent = label;
    b.disabled = !enabled;
    b.addEventListener("click", () => {
      action();
      dismissTerminalMenu();
    });
    menu.appendChild(b);
  };
  addItem(t("term.ctx.copy"), !!sel, () => {
    if (sel) {
      navigator.clipboard
        .writeText(sel)
        .catch((err) => console.warn("terminal copy failed", err));
    }
  });
  addItem(t("term.ctx.paste"), true, () => {
    navigator.clipboard
      .readText()
      .then((text) => {
        if (text) ti.term.paste(text);
        // Phase 65 (bug X): the context menu stole focus; return it to
        // the terminal so the caret stays at the paste site.
        ti.term.focus();
      })
      .catch((err) => console.warn("terminal paste failed", err));
  });
  addItem(t("term.ctx.selectAll"), true, () => ti.term.selectAll());

  // Append first so we can measure, then clamp inside the viewport.
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  const px = Math.max(4, Math.min(x, window.innerWidth - r.width - 8));
  const py = Math.max(4, Math.min(y, window.innerHeight - r.height - 8));
  menu.style.left = `${px}px`;
  menu.style.top = `${py}px`;
  g_termMenu = menu;
  // Capture-phase dismiss so a click anywhere else closes it before
  // that click does anything surprising.
  document.addEventListener("mousedown", onTermMenuOutside, true);
  document.addEventListener("keydown", onTermMenuKey, true);
  window.addEventListener("blur", dismissTerminalMenu);
  window.addEventListener("resize", dismissTerminalMenu);
}

export class TerminalInstance {
  term: Terminal;
  fit: FitAddon;
  container: HTMLDivElement;
  sessionId: string | null = null;
  paneId: string;
  // Phase 62.B (item J): the workspace this pane belongs to, set on
  // connect. Needed so an OSC 8 `file://` link click can SFTP-download
  // from the right remote. null until connected (download needs a live
  // SSH session anyway).
  workspaceId: string | null = null;
  // Phase 62.C (J.1): one-shot diagnostic flag — have we yet seen an
  // OSC 8 hyperlink sequence arrive in this pane's stream? Logged once
  // (metadata only, Rule #1) so we can tell "Claude isn't emitting OSC 8"
  // apart from "our linkHandler isn't firing".
  private oscHyperlinkLogged = false;
  /** The RTL mode active when this terminal was constructed. Changing
   * settings later only affects the data-write pipeline (and the
   * per-row dir attribute observer); the renderer choice is sticky. */
  rtlModeAtConstruct: RtlMode;
  private dataDisposable: IDisposable | null = null;
  // Phase 64 (J, Track B): the plain-text `[file]` link provider handle +
  // a one-shot "it matched" diagnostic flag (metadata only, Rule #1) so a
  // live test can distinguish "regex never matched the real format" from
  // "click path is broken".
  private fileLinkProvider: IDisposable | null = null;
  private fileLinkMatchLogged = false;
  private ro: ResizeObserver | null = null;
  private dirObserver: MutationObserver | null = null;
  // v0.4.4 (RTL Approach C): rAF handle + per-row text cache for the
  // per-line direction pass. The MutationObserver coalesces a burst of cell
  // mutations into a single applyDir() per animation frame; the cache lets
  // that pass skip any row whose text is unchanged since we last set its dir.
  private dirRafId: number | null = null;
  private dirCache: WeakMap<Element, string> = new WeakMap();
  // Phase 23.E: keep a handle to the WebGL addon so we can flush its
  // glyph atlas on resize. Without that, the GPU canvas keeps painting
  // the previous viewport's grid metrics — visible as "stuck" lines
  // that don't reflow when the user drags the pane divider.
  private webglAddon: WebglAddon | null = null;
  // Phase 23.E: rAF-throttle the ResizeObserver fire-rate. During a
  // drag, RO fires per-pixel and each call sends a SIGWINCH down the
  // SSH channel — tmux struggles to keep up and the renderer thrashes.
  // One fit per animation frame is enough; the trailing call after
  // the drag stops is what produces the final correct layout.
  private resizeRafId: number | null = null;
  // Phase 25.B: trailing-edge debounce. The rAF throttle alone is
  // leading-edge — during a FAST drag the very last container size
  // can land between rAF ticks and never get a fit, leaving the
  // terminal "stuck" at an intermediate width until the user
  // re-focuses the pane. This timer fires one authoritative fit
  // ~140ms after the resize storm ends, guaranteeing the final
  // dimensions are always applied.
  private resizeSettleTimer: number | null = null;
  // Phase 35 (#1.1): rAF-coalesced PTY writer. During fast streaming
  // (Claude generating, a noisy build), the backend fires many small
  // pty:data events per frame. Calling term.write() on each one forces
  // xterm to reflow/repaint per chunk and starves the event loop —
  // the window shows "(Not Responding)". Instead we accumulate chunks
  // and flush a single merged write per animation frame.
  private pendingChunks: string[] = [];
  private flushRafId: number | null = null;
  // v0.4.4-beta.4 (RTL mouse fix): capture-phase mouse listener disposer.
  // The listener runs before xterm.js's own bubble-phase handlers and, for
  // events over an RTL row, dispatches a synthetic MouseEvent with clientX
  // mirrored around the row midpoint. See mouseRtl.ts for the rationale.
  private rtlMouseTeardown: (() => void) | null = null;

  constructor(paneId: string) {
    this.paneId = paneId;
    this.rtlModeAtConstruct = g_rtlMode;
    this.container = document.createElement("div");
    this.container.className = "terminal-container";

    this.term = new Terminal({
      fontFamily:
        g_fontFamily ??
        '"Cascadia Mono", "JetBrains Mono", Consolas, "Courier New", monospace',
      fontSize: g_fontSizePx ?? 14,
      lineHeight: 1.15,
      cursorBlink: true,
      cursorStyle: "bar",
      cursorWidth: 2,
      allowProposedApi: true,
      allowTransparency: true,
      theme: g_termTheme ?? DEFAULT_TERM_THEME,
      // Redesign pass 6: apps pick 256/truecolor foregrounds assuming a
      // dark ground (Claude Code's dim-gray bullets vanish on the light
      // themes). The 16 ANSI slots are theme-mapped, but indexed/true
      // colors can't be — so let xterm auto-adjust any foreground that
      // misses WCAG AA contrast against the background (VS Code's
      // default terminal behaviour, same 4.5 ratio).
      minimumContrastRatio: 4.5,
      scrollback: 10000,
      windowsPty: { backend: "conpty" },
      windowOptions: { setWinSizeChars: true },
      // Phase 62.B (item J): handle OSC 8 hyperlinks. Claude Code emits
      // file:// links for files it produces; clicking one SFTP-downloads
      // it to the user's Downloads folder. http(s) links open in the
      // system browser. allowNonHttpProtocols lets xterm render the
      // file:// runs as clickable links at all.
      linkHandler: {
        allowNonHttpProtocols: true,
        activate: (_event: MouseEvent, uri: string) => {
          const filePath = fileUriToPath(uri);
          if (filePath !== null) {
            // Dispatch to App, which owns the toast + the download invoke.
            window.dispatchEvent(
              new CustomEvent("winmux:osc-file-link", {
                detail: { workspaceId: this.workspaceId, path: filePath },
              }),
            );
            return;
          }
          if (/^https?:\/\//i.test(uri)) {
            void openUrl(uri).catch((e) => console.warn("openUrl failed", e));
          }
        },
        hover: (_event: MouseEvent, uri: string) => {
          this.container.title = uri;
        },
        leave: () => {
          this.container.removeAttribute("title");
        },
      },
      // Phase 55-C: convertEol stays FALSE. Despite occasional
      // complaints that "newlines look wrong after a resize," flipping
      // this to true would double every CRLF that ConPTY (and every
      // modern PTY) already emits, because convertEol rewrites LF →
      // CRLF on the INPUT stream regardless of what's there. The
      // reflow problem is structural to scrollback rasterisation —
      // see docs/CONTRIBUTING.md "Scrollback reflow is fundamentally
      // limited" for the full background.
      convertEol: false,
      // Phase 23.E: explicit reflow=true. xterm.js's default is true,
      // but if a previous setting drifted, scrollback wouldn't rewrap
      // when the pane is resized — text appears "stuck" at the
      // pre-resize column width. Belt-and-suspenders.
      // (Property removed in xterm v5+; if the type errors leave it
      // out — we still get reflow because that's now the unconditional
      // behaviour.)
    });
    this.fit = new FitAddon();
    this.term.loadAddon(this.fit);
    // Phase LL: OSC 52 clipboard support. Claude Code's new fullscreen
    // renderer copies the selection by emitting an OSC 52 escape sequence
    // (the alt-screen + SGR-mouse mode steals drag-selection, so OSC 52 is
    // how it reaches the system clipboard). xterm.js ignores OSC 52 without
    // this addon — which is exactly why "copy didn't work" in fullscreen.
    this.term.loadAddon(new ClipboardAddon(undefined, g_oscClipboardProvider));
    // Phase 64 (J, Track B): make Claude's plain-text `[file] <path> (<size>)`
    // lines clickable. Absolute paths reuse the existing OSC 8 file-link
    // download path (Save-As dialog via App); relative paths can't be
    // resolved — winmux has no per-pane remote cwd (OSC 7 tracking is the
    // future prerequisite) — so the click copies the path and a toast says
    // it's relative to the pane's directory.
    this.fileLinkProvider = this.term.registerLinkProvider({
      provideLinks: (y, callback) => {
        const line = this.term.buffer.active.getLine(y - 1);
        if (!line) return callback(undefined);
        const text = line.translateToString(true);
        let links: ILink[] | undefined;
        FILE_LINK_RE.lastIndex = 0;
        for (let m = FILE_LINK_RE.exec(text); m; m = FILE_LINK_RE.exec(text)) {
          const path = m[1];
          if (!this.fileLinkMatchLogged) {
            this.fileLinkMatchLogged = true;
            void invoke("diag_log", {
              level: "info",
              msg: `[file] link provider matched in pane ${this.paneId}`,
            }).catch(() => {});
          }
          (links ??= []).push({
            // xterm buffer ranges are 1-based with an inclusive end cell.
            range: {
              start: { x: m.index + 1, y },
              end: { x: m.index + m[0].length, y },
            },
            text: m[0],
            activate: () => {
              if (path.startsWith("/")) {
                window.dispatchEvent(
                  new CustomEvent("winmux:osc-file-link", {
                    detail: { workspaceId: this.workspaceId, path },
                  }),
                );
              } else {
                window.dispatchEvent(
                  new CustomEvent("winmux:file-link-relative", {
                    detail: { path },
                  }),
                );
              }
            },
            hover: () => {
              this.container.title = path;
            },
            leave: () => {
              this.container.removeAttribute("title");
            },
          });
        }
        callback(links);
      },
    });
    this.term.open(this.container);

    // Phase 16: custom key handler. When the user presses plain
    // Ctrl+C with a non-empty selection AND the setting is enabled,
    // copy to clipboard + suppress the keystroke (so the shell never
    // sees a SIGINT). All other keystrokes fall through unchanged.
    // Returning `false` from `attachCustomKeyEventHandler` tells
    // xterm.js NOT to forward the event to the PTY.
    this.term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      if (
        g_ctrlCCopyOnSelect &&
        e.ctrlKey &&
        !e.shiftKey &&
        !e.altKey &&
        !e.metaKey &&
        (e.key === "c" || e.key === "C")
      ) {
        const sel = this.term.getSelection();
        if (sel) {
          navigator.clipboard.writeText(sel).catch((err) =>
            console.warn("ctrl-c copy failed", err)
          );
          return false; // swallow — don't send SIGINT
        }
      }
      return true;
    });

    // Phase 60 → 62.A (item E): right-click opens a custom Copy / Paste
    // / Select-all menu. Phase 60 suppressed the native WebView2 menu
    // and made right-click paste-only; in the plain terminal that lost
    // the user's Copy affordance entirely ("copy and paste stopped
    // working"). We keep the native menu suppressed but give back a
    // real menu. The full mouse contract is now:
    //   left-drag      → native xterm selection
    //   Ctrl+C w/ sel  → copy (copy-on-select setting, above)
    //   Ctrl+Shift+C   → copy (global shortcut table)
    //   Ctrl+Shift+V   → paste (global shortcut table)
    //   right-click    → Copy / Paste / Select-all menu
    this.container.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      showTerminalContextMenu(this, e.clientX, e.clientY);
    });

    // Phase 65.O (round 6 — final): NO custom wheel handler. Earlier
    // rounds intercepted the wheel and injected Alt+arrows to drive tmux
    // copy-mode, but that fought xterm.js's native behaviour and broke
    // the common case — Yossi's `TMUX=`(empty) / `#{mouse}`=0 diag showed
    // the proxy was firing in a PLAIN bash shell (not even tmux), sending
    // Alt+Up that bash read as history navigation. xterm.js's built-in
    // wheel handling already does the right thing everywhere:
    //   - plain shell (no tmux)      → scrolls xterm.js's own scrollback
    //   - tmux + `mouse on`          → emits SGR mouse events; tmux scrolls
    //   - tmux + `mouse off`         → scrolls xterm.js's scrollback
    // So we simply let it be. `scrollback` is set in the Terminal options
    // above; the bundled tmux.conf ships `mouse on` for native tmux
    // scroll. (One-time note in the console for future debugging.)
    if (!g_loggedNoWheelProxy) {
      g_loggedNoWheelProxy = true;
      console.log(
        "[winmux] terminal: native wheel scrollback enabled, no wheel proxy",
      );
    }

    // Phase 15.A: only load the WebGL addon for the non-auto modes.
    // `auto_per_line` needs the DOM renderer so we can attach
    // dir="auto" per row. WebGL paints to a canvas and has no per-cell
    // DOM, so the browser BiDi engine has nothing to hook into.
    if (this.rtlModeAtConstruct !== "auto_per_line") {
      try {
        const addon = new WebglAddon();
        // Phase 25: WebGL contexts can be lost — GPU driver resets,
        // memory pressure, or an aggressive resize. When that happens
        // the canvas goes permanently blank unless we react. Disposing
        // the addon makes xterm.js fall back to the DOM renderer,
        // which is slower but always renders. Without this handler a
        // lost context = a dead-blank terminal with no recovery
        // (the "terminal goes blank after resizing post-conversation"
        // bug).
        addon.onContextLoss(() => {
          console.warn("WebGL context lost — falling back to DOM renderer");
          try {
            addon.dispose();
          } catch {}
          this.webglAddon = null;
          // Force a full repaint on the DOM renderer that xterm.js
          // now falls back to.
          try {
            this.term.refresh(0, this.term.rows - 1);
          } catch {}
        });
        this.term.loadAddon(addon);
        this.webglAddon = addon;
      } catch (e) {
        console.warn("WebGL addon unavailable", e);
      }
    }

    // Phase 23.E + 25.B: resize handling has two layers.
    //  - rAF throttle: smooth live updates during the drag without
    //    flooding SIGWINCH down the SSH channel.
    //  - trailing settle timer: fires ONE authoritative fit ~140ms
    //    after the last resize event, so the final container size is
    //    always applied even if a fast drag's last delta landed
    //    between rAF ticks. Fixes "terminal stuck at an intermediate
    //    width until I re-focus the pane".
    this.ro = new ResizeObserver(() => {
      if (this.resizeRafId == null) {
        this.resizeRafId = requestAnimationFrame(() => {
          this.resizeRafId = null;
          this.fitAndResize();
        });
      }
      if (this.resizeSettleTimer != null) {
        clearTimeout(this.resizeSettleTimer);
      }
      this.resizeSettleTimer = window.setTimeout(() => {
        this.resizeSettleTimer = null;
        // Phase 25.C: force=true — always re-send the settled
        // dimensions to tmux even if xterm.js thinks nothing changed.
        this.fitAndResize(true);
      }, 120);
    });
    this.ro.observe(this.container);
    g_terminals.add(this);

    this.ensureDirObserver();
    this.installRtlMouseCapture();

    // Font-init fix: a fresh pane rendered "compressed" until the user
    // swapped the terminal font and back (notably with Courier). Root
    // cause: xterm caches the character-cell size from its FIRST
    // measurement at `term.open()` above — but at construction the
    // container is still detached from the DOM and the web font may not
    // be loaded yet, so the cached cell is wrong and every later `fit()`
    // (which only divides the container size by that stale cell) inherits
    // the bad metrics. Schedule a one-shot re-measure once the container
    // is actually in the DOM and fonts are ready.
    this.scheduleInitialFontMeasure();
  }

  /**
   * v0.4.4-beta.4 (RTL mouse fix): capture-phase listeners on the terminal
   * element that mirror `clientX` for events landing over an RTL row. See
   * mouseRtl.ts for the math and the docs on why we intercept in capture
   * phase (xterm.js binds its own mousedown to `this.element` in bubble
   * phase, and its drag lifecycle attaches mousemove/mouseup to `document`
   * -- also bubble). Capture on `this.container` fires before either.
   *
   * The listener is a no-op when the pointer is over an LTR row, over no
   * row (whitespace / outside `.xterm-rows`), or when the DOM renderer
   * isn't being used (the `dir` attributes only get set in `auto_per_line`
   * mode with the DOM renderer -- the WebGL renderer has no per-row DOM).
   *
   * Re-entry: `dispatchEvent` re-enters the capture phase, so we gate on
   * `event.isTrusted` to skip synthetic events we ourselves fired.
   */
  private installRtlMouseCapture(): void {
    if (this.rtlModeAtConstruct !== "auto_per_line") return;
    const el = this.container;

    const forward = (e: MouseEvent): void => {
      // Skip synthetic events we dispatched (re-entry guard). Only real
      // OS-generated events are trusted.
      if (!e.isTrusted) return;
      const rowsHost = el.querySelector(".xterm-rows") as HTMLElement | null;
      if (!rowsHost) return;
      const row = findRow(rowsHost, e.clientY);
      if (!row || row.dir !== "rtl") return;
      const newX = transformMouseX(e.clientX, row);
      if (newX === e.clientX) return;

      // Suppress the original event before it reaches xterm's own handlers
      // (bubble phase on this.element for mousedown, on document for
      // mousemove/mouseup during a drag). stopPropagation is enough --
      // stopImmediatePropagation would also block any OTHER capture
      // listener on the way down, which we don't need to do.
      e.stopPropagation();
      // preventDefault so the browser doesn't also start a native text
      // selection over the mirrored coord (which would clash with xterm's).
      e.preventDefault();

      const target = (e.target as EventTarget | null) ?? el;
      const clone = new MouseEvent(e.type, {
        bubbles: true,
        cancelable: true,
        composed: true,
        button: e.button,
        buttons: e.buttons,
        clientX: newX,
        clientY: e.clientY,
        screenX: e.screenX + (newX - e.clientX),
        screenY: e.screenY,
        ctrlKey: e.ctrlKey,
        shiftKey: e.shiftKey,
        altKey: e.altKey,
        metaKey: e.metaKey,
        detail: e.detail,
        view: window,
      });
      target.dispatchEvent(clone);
    };

    // Deliberately NOT included:
    //  - `contextmenu`: the existing handler in the constructor positions
    //    the custom Copy/Paste menu at the raw clientX. Mirroring would put
    //    the menu on the "wrong" (visual-opposite) side of the click.
    //  - `wheel`: no per-column meaning; xterm's wheel handling is scroll-
    //    based, not cell-based.
    const events: Array<keyof HTMLElementEventMap> = [
      "mousedown",
      "mousemove",
      "mouseup",
      "click",
      "dblclick",
    ];
    for (const ev of events) {
      el.addEventListener(ev, forward as EventListener, true);
    }
    // Drag past the terminal edge: xterm's SelectionService binds mousemove
    // and mouseup on `document` for the drag lifecycle. When the pointer
    // stays inside the terminal those events also propagate through
    // `this.container`, so the listeners above catch them. But if the
    // pointer LEAVES the terminal (drag out, release outside), the target
    // is no longer under `this.container` and only the document-level
    // listener would need mirroring. We accept the minor drop-off there --
    // most drags start and end inside the pane, and mirroring outside-row
    // coords would require guessing which row the pointer "would have"
    // hit. Documented as a known limitation.

    this.rtlMouseTeardown = () => {
      for (const ev of events) {
        el.removeEventListener(ev, forward as EventListener, true);
      }
    };
  }

  /**
   * v0.4.4 (RTL Approach C): give every VISIBLE row div under `.xterm-rows`
   * an explicit `dir` computed from its text by detectDirection (mixed→RTL,
   * pure-Latin→LTR), instead of the old `dir="auto"` (which used the browser's
   * "first strong char wins" rule and mis-rendered mixed lines starting with
   * Latin). xterm.js's DOM renderer recycles its row divs as the buffer
   * scrolls and rewrites their text in place, so a MutationObserver
   * (childList + characterData) re-triggers the pass; it is coalesced to one
   * run per animation frame and a per-row text cache skips unchanged rows.
   * Only visible rows carry DOM nodes, so scrollback size is irrelevant.
   */
  ensureDirObserver(): void {
    // Only relevant in auto-per-line mode AND when we built with the
    // DOM renderer. In any other mode, drop any existing observer.
    if (this.rtlModeAtConstruct !== "auto_per_line") {
      if (this.dirObserver) {
        this.dirObserver.disconnect();
        this.dirObserver = null;
      }
      return;
    }
    if (this.dirObserver) return;

    const rowsHost = this.container.querySelector(".xterm-rows") as HTMLElement | null;
    if (!rowsHost) {
      // Renderer not mounted yet — retry on the next animation frame.
      requestAnimationFrame(() => this.ensureDirObserver());
      return;
    }
    // The rows host stays LTR so the grid geometry (column origin) is stable;
    // only the per-row paragraph direction flips.
    rowsHost.setAttribute("dir", "ltr");
    this.applyRowDirections(true);
    const obs = new MutationObserver(() => this.scheduleRowDirections());
    obs.observe(rowsHost, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    this.dirObserver = obs;
  }

  /** Coalesce a burst of row mutations into one direction pass per frame. */
  private scheduleRowDirections(): void {
    if (this.dirRafId != null) return;
    this.dirRafId = requestAnimationFrame(() => {
      this.dirRafId = null;
      this.applyRowDirections(false);
    });
  }

  /**
   * Set `dir` on each visible row from its text.
   *
   * `force` recomputes every row (used on first attach and when the setting
   * toggles); otherwise the per-row cache skips rows whose text is unchanged
   * AND whose *neighbors* haven't shifted the block classification. When any
   * row's text changes we run the full block-aware pass ({@link
   * detectRowDirections}) — so a Hebrew cell landing in a table drags the
   * whole block RTL, and adding a border row that groups earlier content rows
   * into a new block re-flows their direction too.
   *
   * `detectDirection` (single-row) is retained above only as a fallback for
   * `auto_direction=false` (see below).
   */
  applyRowDirections(force: boolean): void {
    if (this.rtlModeAtConstruct !== "auto_per_line") return;
    const rowsHost = this.container.querySelector(".xterm-rows") as HTMLElement | null;
    if (!rowsHost) return;
    const auto = g_autoDirection;
    const children = Array.from(rowsHost.children) as HTMLElement[];
    const texts = children.map((el) => el.textContent ?? "");

    // Fast path: if not forced AND every row text is unchanged since last
    // pass, we can skip the whole recompute. Block classification depends on
    // ALL rows, so a per-row cache with per-row skip (as before) would race
    // with block-boundary shifts. Cheap array-equality check instead.
    if (!force) {
      let allSame = children.length > 0;
      for (let i = 0; i < children.length; i++) {
        if (this.dirCache.get(children[i]) !== texts[i]) { allSame = false; break; }
      }
      if (allSame) return;
    }

    const dirs = auto
      ? detectRowDirections(texts)
      : (texts.map(() => "ltr") as ("ltr" | "rtl")[]);

    for (let i = 0; i < children.length; i++) {
      const el = children[i];
      this.dirCache.set(el, texts[i]);
      const dir = dirs[i];
      if (el.getAttribute("dir") !== dir) el.setAttribute("dir", dir);
    }
  }

  attach(sessionId: string) {
    this.detach();
    this.sessionId = sessionId;
    this.dataDisposable = this.term.onData((data) => {
      let out = data;
      // Phase HH: on an RTL line, the visual Left/Right arrows map to the
      // opposite logical direction, so mirror them. Only the 4 horizontal
      // cursor-key sequences are considered, and only when the cursor's
      // line is predominantly RTL — LTR lines pass through untouched.
      if (
        g_mirrorArrowsRtl &&
        (data === "\x1b[C" ||
          data === "\x1b[D" ||
          data === "\x1bOC" ||
          data === "\x1bOD") &&
        this.isCurrentLineRtl()
      ) {
        out = swapArrowSeq(data);
      }
      if (this.sessionId)
        invoke("pty_write", { sessionId: this.sessionId, data: out }).catch(
          (err) => console.error("pty_write failed", err)
        );
    });
    // Phase 25.C: force a pty_resize on attach so tmux gets the
    // current dimensions immediately, even on a reconnect where
    // xterm.js's cols/rows happen to match the previous session.
    this.fitAndResize(true);
    // v0.4.4-beta.2: clear any stale mouse-tracking state inherited from a
    // previous session on this instance (a reconnect where an app left SGR
    // mouse mode on). Fresh instances start clean; this is the reconnect
    // safety net. Gated by the Settings toggle.
    if (g_autoResetOnConnect) this.resetMouseModes();
  }

  /** v0.4.4-beta.2: tell xterm.js to disable every mouse-tracking mode.
   *  Writes DECRST sequences to the DISPLAY (not the PTY), so xterm stops
   *  emitting mouse events even if the app that turned them on is gone. */
  resetMouseModes(): void {
    try {
      this.term.write(MOUSE_DISABLE_SEQ);
    } catch (e) {
      console.warn("resetMouseModes failed", e);
    }
  }

  /** v0.4.4-beta.2: manual "Reset terminal" — disable mouse tracking +
   *  reset text attributes (SGR). Used by the Ctrl+Alt+R command. Does NOT
   *  clear the screen/scrollback (no RIS), so it's safe to run any time. */
  resetTerminal(): void {
    try {
      this.term.write(MOUSE_DISABLE_SEQ + "\x1b[0m");
    } catch (e) {
      console.warn("resetTerminal failed", e);
    }
  }

  /** Phase HH: is the terminal line under the cursor predominantly RTL?
   *  Reads the live buffer line at the absolute cursor row. Best-effort —
   *  any xterm API hiccup falls back to "not RTL" (no mirroring). */
  private isCurrentLineRtl(): boolean {
    try {
      const buf = this.term.buffer.active;
      const line = buf.getLine(buf.baseY + buf.cursorY);
      if (!line) return false;
      // v0.4.4: use the SAME rule as the per-line display direction
      // (detectDirection) so the caret/arrow behavior matches what the user
      // sees — a line rendered RTL also gets its Left/Right arrows mirrored.
      // Candidate fix for the PARKED "RTL caret" item (verify live).
      return detectDirection(line.translateToString(true)) === "rtl";
    } catch {
      return false;
    }
  }

  detach() {
    this.dataDisposable?.dispose();
    this.dataDisposable = null;
    this.sessionId = null;
  }

  fitAndResize(force = false) {
    const prevCols = this.term.cols;
    const prevRows = this.term.rows;
    try {
      this.fit.fit();
    } catch {}
    const changed = this.term.cols !== prevCols || this.term.rows !== prevRows;
    // Phase 25.C: when `force` is set (the trailing settle fit after
    // a resize storm, or attach() on a reconnect), ALWAYS push
    // pty_resize to the remote even if xterm.js's own cols/rows
    // didn't change since the last fit. An intermediate fit during a
    // fast drag can update xterm.js to the final size and fire a
    // pty_resize that races / never reaches tmux; the settle fit
    // would then see `changed=false` and skip pty_resize, leaving
    // tmux painting at the stale width. Forcing the resize
    // guarantees tmux is told the final dimensions.
    if (this.sessionId && (changed || force)) {
      invoke("pty_resize", {
        sessionId: this.sessionId,
        cols: this.term.cols,
        rows: this.term.rows,
      }).catch(() => {});
    }
    // Phase 25: force a repaint after a real dimension change so the
    // renderer picks up the new grid metrics.
    //
    // NOTE: Phase 23.E also called `webglAddon.clearTextureAtlas()`
    // here. That turned out to be the cause of the "terminal goes
    // blank after resizing once a conversation has filled the
    // scrollback" bug — wiping the glyph atlas mid-resize, with a
    // large reflowed scrollback, could leave the WebGL canvas unable
    // to re-rasterize and stuck blank. The atlas is invalidated
    // internally by the WebGL addon on resize anyway, so the manual
    // call was both redundant and harmful. Removed. A plain
    // `term.refresh()` is enough to repaint the viewport, and the
    // onContextLoss handler above covers the case where WebGL does
    // die.
    // Phase 25.C: force=true also triggers a repaint so the settled
    // fit guarantees a fresh viewport even when grid metrics didn't
    // change.
    if (changed || force) {
      try {
        this.term.refresh(0, this.term.rows - 1);
      } catch {}
    }
  }

  /** One-shot guard for {@link scheduleInitialFontMeasure}. */
  private fontMeasured = false;

  private cs(): NonNullable<
    NonNullable<XtermInternals["_core"]>["_charSizeService"]
  > | undefined {
    return (this.term as unknown as XtermInternals)._core?._charSizeService;
  }

  /** Diagnostic: log this pane's measured cell + applied font (Rule #1:
   *  metrics only). Public so the module-level setTerminalFont can trace the
   *  manual font-swap path. */
  logFontSwap(label: string): void {
    const cs = this.cs();
    const fam = String(this.term.options.fontFamily ?? "").slice(0, 40);
    void invoke("diag_log", {
      level: "info",
      msg: `[font-swap] ${label} pane=${this.paneId} charSvc=${cs?.width}x${cs?.height} size=${this.term.options.fontSize} fam=${JSON.stringify(fam)}`,
    }).catch(() => {});
  }

  /** Apply a font family + size to THIS terminal exactly the way
   *  setTerminalFont does per-instance (option set + fit + refresh). */
  private applyFontOnce(family: string, px: number): void {
    this.term.options.fontFamily = family;
    this.term.options.fontSize = px;
    this.fitAndResize();
    try {
      this.term.refresh(0, this.term.rows - 1);
    } catch {}
  }

  /**
   * Re-measure the cell by REPLICATING the manual Settings font-swap — the
   * only thing proven to un-stick a bad measurement. Proven from the debug
   * log: a fresh pane on the bitmap font `Courier 10,12,15 (120)` measures the
   * cell "cold" at 9.44x11 (half-height → compressed). Re-applying the SAME
   * family, or calling CharSizeService.measure() directly, leaves it at
   * 9.44x11. But applying a DIFFERENT scalable family first and THEN restoring
   * the user's family lands on the correct 12.0x23 and stays there — the
   * intermediate scalable application forces the browser to re-resolve the
   * cached cell. The final family is always the user's own setting
   * (`g_fontFamily`); the intermediate is a throwaway unsticker, so whatever
   * the user's family resolves to is unchanged from the manual swap-back.
   *
   * Timing matters: the working manual swap had the intermediate font actually
   * painted before the swap-back, so we restore the real family after a DOUBLE
   * requestAnimationFrame (one full paint of the intermediate), not the next
   * frame. The self-verify loop in scheduleInitialFontMeasure retries if a
   * single pass doesn't stick.
   */
  remeasureFont(): void {
    const real =
      g_fontFamily ?? String(this.term.options.fontFamily ?? "monospace");
    const px = g_fontSizePx ?? Number(this.term.options.fontSize ?? 14);
    // Always-present Windows system scalable fonts → resolve synchronously,
    // no web-font load race. Only needs to differ from `real` to trigger the
    // re-resolution.
    const unsticker = '"Consolas", "Courier New", monospace';
    const intermediate = real === unsticker ? "monospace" : unsticker;
    this.applyFontOnce(intermediate, px);
    requestAnimationFrame(() => {
      if (!g_terminals.has(this)) return;
      // Second frame: the intermediate has now painted at least once.
      requestAnimationFrame(() => {
        if (!g_terminals.has(this)) return;
        this.applyFontOnce(real, px);
        const cs = this.cs();
        void invoke("diag_log", {
          level: "info",
          msg: `[font-fix] pane=${this.paneId} afterSwap charSvc=${cs?.width}x${cs?.height} size=${this.term.options.fontSize}`,
        }).catch(() => {});
      });
    });
  }

  /** True if the cell isn't measured yet, or is "compressed" — height well
   *  under the font size (the bug). A correct cell is ≈ `fontSize * 1.15+`. */
  private cellNeedsRemeasure(): boolean {
    const h = this.cs()?.height;
    if (typeof h !== "number") return true;
    const size = Number(this.term.options.fontSize ?? 14);
    return h < size * 0.9;
  }

  /**
   * Font-init fix: once this pane's container is in the DOM, re-measure the
   * font (see {@link remeasureFont}). Runs once when connected, then a few
   * more times over ~1s to beat the web-font load race — the intended face
   * may still be loading at first paint, and re-measuring after it lands
   * pins the correct cell size (the same thing re-picking the font in
   * Settings does). Extra passes are cheap and idempotent; the
   * `g_terminals` check stops them if the pane is disposed meanwhile.
   */
  private scheduleInitialFontMeasure(): void {
    const fonts =
      typeof document !== "undefined" ? document.fonts : undefined;

    // Force the real candidate faces to actually LOAD — document.fonts.ready
    // alone doesn't, since a face isn't "pending" until requested. Skips CSS
    // generics and malformed comma-split junk. Resolving means the intended
    // face is loaded, so the next measure() lands on it.
    const loadFaces = (): Promise<unknown> => {
      if (!fonts || typeof fonts.load !== "function") return Promise.resolve();
      const size = Number(this.term.options.fontSize ?? 14);
      const families = String(this.term.options.fontFamily ?? "")
        .split(",")
        .map((f) => f.trim().replace(/^["']|["']$/g, ""))
        .filter(
          (f) =>
            f.length > 0 &&
            !/^(monospace|serif|sans-serif|ui-monospace|system-ui|cursive|fantasy)$/i.test(
              f,
            ) &&
            /^[\w .-]+$/.test(f),
        );
      return Promise.allSettled(
        families.map((f) => fonts.load(`${size}px "${f}"`)),
      );
    };

    // Self-verifying loop: call xterm's own measure() until the cell is no
    // longer compressed, or a ceiling. Because measure() actually re-measures
    // and we CHECK the result (cellNeedsRemeasure via charSvc), this can't
    // silently no-op and it naturally waits out the font-load race. Stops the
    // instant the cell is correct.
    let attempts = 0;
    const MAX_ATTEMPTS = 15; // ~3s at 200ms
    const step = () => {
      if (!g_terminals.has(this) || !this.container.isConnected) return;
      if (!this.cellNeedsRemeasure()) return; // correct → done
      if (attempts >= MAX_ATTEMPTS) return;
      attempts += 1;
      this.remeasureFont();
      window.setTimeout(step, 200);
    };

    const run = () => {
      if (this.fontMeasured || !g_terminals.has(this)) return;
      if (!this.container.isConnected) {
        // PaneView hasn't appended the container to its slot yet.
        requestAnimationFrame(run);
        return;
      }
      this.fontMeasured = true;
      step();
      void loadFaces().finally(() => {
        if (g_terminals.has(this) && this.cellNeedsRemeasure()) step();
      });
    };
    requestAnimationFrame(run);
  }

  writeData(data: string) {
    // Phase 35: queue and coalesce. Merging chunks before the reorder
    // pipeline is also more correct than per-chunk - a chunk boundary
    // that splits a line or escape sequence now gets reassembled before
    // reorderRtlForDisplay sees it.
    this.pendingChunks.push(data);
    if (this.flushRafId === null) {
      this.flushRafId = requestAnimationFrame(() => this.flushPending());
    }
  }

  private flushPending() {
    this.flushRafId = null;
    if (this.pendingChunks.length === 0) return;
    const merged = this.pendingChunks.join("");
    this.pendingChunks = [];
    // Phase 62.C (J.1): record (once, metadata only - Rule #1) whether
    // OSC 8 hyperlink sequences (ESC ] 8 ;) actually reach this pane. If
    // the debug.log never shows this line while Claude prints file links,
    // the sequences are being stripped upstream (or Claude isn't emitting
    // them) - not a linkHandler bug.
    if (!this.oscHyperlinkLogged && merged.includes("]8;")) {
      this.oscHyperlinkLogged = true;
      void invoke("diag_log", {
        level: "info",
        msg: `OSC8 hyperlink sequence detected in pane ${this.paneId}`,
      }).catch(() => {});
    }
    // The reorder pipeline keys off the LIVE rtl mode (g_rtlMode), so
    // a settings change takes effect on the very next flush - no
    // need to wait for a new pane.
    if (g_rtlMode === "bidi_reorder") {
      this.term.write(reorderRtlForDisplay(merged));
    } else {
      this.term.write(merged);
    }
  }

  notice(msg: string) {
    this.term.writeln(`\r\n\x1b[33m${msg}\x1b[0m`);
  }

  focus() {
    this.term.focus();
  }

  dispose() {
    // Phase 62.A (item E): close the right-click menu if it's open over
    // this terminal - its actions reference this.term.
    dismissTerminalMenu();
    // Phase 35: flush any queued PTY chunks synchronously before the
    // rAF can fire, so the last bytes aren't lost when a pane closes
    // mid-stream. Then cancel the pending frame.
    if (this.flushRafId != null) {
      cancelAnimationFrame(this.flushRafId);
      this.flushRafId = null;
    }
    try {
      this.flushPending();
    } catch {}
    if (this.resizeRafId != null) {
      cancelAnimationFrame(this.resizeRafId);
      this.resizeRafId = null;
    }
    // Phase 25.B: cancel the trailing settle timer so a freed
    // terminal doesn't fire a fit after disposal.
    if (this.resizeSettleTimer != null) {
      clearTimeout(this.resizeSettleTimer);
      this.resizeSettleTimer = null;
    }
    this.ro?.disconnect();
    this.ro = null;
    this.fileLinkProvider?.dispose();
    this.fileLinkProvider = null;
    this.dirObserver?.disconnect();
    this.dirObserver = null;
    // v0.4.4-beta.4 (RTL mouse fix): tear down the capture-phase listener
    // so a disposed pane doesn't keep intercepting document/element mouse
    // events for the rest of the app's lifetime.
    this.rtlMouseTeardown?.();
    this.rtlMouseTeardown = null;
    // v0.4.4: cancel a pending per-line direction pass so a freed terminal
    // doesn't touch detached DOM after disposal.
    if (this.dirRafId != null) {
      cancelAnimationFrame(this.dirRafId);
      this.dirRafId = null;
    }
    this.detach();
    g_terminals.delete(this);
    // Phase 25: release the WebGL addon BEFORE term.dispose() so we
    // can swallow any teardown error specific to the GPU canvas
    // without the rest of dispose() being skipped. Also reads the
    // field, which keeps tsc happy now that the in-flight
    // clearTextureAtlas reader is gone (Phase 23.E).
    try {
      this.webglAddon?.dispose();
    } catch {}
    this.webglAddon = null;
    this.term.dispose();
    if (this.container.parentElement) {
      this.container.parentElement.removeChild(this.container);
    }
  }
}
