import { Terminal, type IDisposable } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { reorderRtlForDisplay } from "./bidi";
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
let g_fontFamily: string | null = null;
let g_fontSizePx: number | null = null;
let g_rtlMode: RtlMode = "auto_per_line";
/** Phase 16: when true, Ctrl+C with a non-empty selection copies the
 *  selection to clipboard instead of sending SIGINT. Mirrors Windows
 *  Terminal / VS Code's behavior. Settings → Shortcuts toggles it. */
let g_ctrlCCopyOnSelect = true;
/** Phase 65.O: when true (and the pane is a tmux session — see
 *  `TerminalInstance.tmuxScrollProxy`), the mouse wheel over the
 *  alternate screen is intercepted and re-sent as Alt+Up/Down instead
 *  of letting xterm.js's alt-scroll emit bare arrows. The bundled
 *  winmux tmux.conf binds Alt+Up/Down to copy-mode scroll at a shell
 *  prompt. Mirrors `use_winmux_tmux_config` (the two are a matched
 *  pair: our interception only makes sense with our conf's bindings). */
let g_winmuxTmuxWheelScroll = true;
const g_terminals: Set<TerminalInstance> = new Set();

/**
 * Push a new font family + size into every live xterm and remember the
 * values so future TerminalInstance constructions inherit them. Family
 * is passed through `quoteFamily()`-style fallbacks already by the
 * caller. Called from App.tsx on settings load and on every
 * `settings:changed`.
 */
export function setTerminalFont(family: string, sizePt: number): void {
  const px = Math.max(8, Math.round(sizePt * PT_TO_PX));
  g_fontFamily = family;
  g_fontSizePx = px;
  for (const ti of g_terminals) {
    try {
      ti.term.options.fontFamily = family;
      ti.term.options.fontSize = px;
      ti.fitAndResize();
      ti.term.refresh(0, ti.term.rows - 1);
    } catch (e) {
      console.warn("setTerminalFont: per-instance update failed", e);
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

/** Phase 16: flip the Ctrl+C-copies-on-selection behavior at runtime. */
export function setCtrlCCopyOnSelect(enabled: boolean): void {
  g_ctrlCCopyOnSelect = enabled;
}

/** Phase 65.O: flip the tmux wheel→copy-mode proxy at runtime. Wired to
 *  the `terminal.use_winmux_tmux_config` setting from App.tsx. */
export function setWinmuxTmuxWheelScroll(enabled: boolean): void {
  g_winmuxTmuxWheelScroll = enabled;
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
  // Phase 65.O: true when this pane is attached to a tmux session, so
  // the wheel→Alt+Up/Down copy-mode proxy should run. Set by App.tsx
  // from the pane-persistence map. Stays false for plain (non-tmux)
  // panes — a local `vim` owns its own alt-screen with no tmux to
  // translate the proxy keys, so those keep xterm's default alt-scroll.
  tmuxScrollProxy = false;
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
  private ro: ResizeObserver | null = null;
  private dirObserver: MutationObserver | null = null;
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
      theme: {
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
      },
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

    // Phase 65.O: tmux scrollback via the mouse wheel while keeping
    // tmux `mouse off` (so native left-drag selection still works).
    // Because tmux owns the alternate screen the whole time it runs,
    // xterm.js's built-in alternate-scroll would turn each wheel tick
    // into a bare Up/Down arrow — at a shell prompt that's history
    // navigation, the v0.2.8 regression. Instead we send Alt+Up/Down,
    // which the bundled winmux tmux.conf binds to copy-mode scroll at a
    // shell prompt (and to plain arrows inside an alt-screen app like
    // vim/less). Gated on `tmuxScrollProxy` so non-tmux panes (a local
    // vim with no tmux to translate the proxy) keep xterm's default; and
    // on the normal buffer we don't touch the wheel at all (real xterm
    // scrollback handles it).
    //
    // CAPTURE PHASE (bug O round 2): xterm.js attaches its own wheel
    // handler to the `.xterm-viewport` child, which fires on the bubble
    // path BEFORE this container-level handler would in bubble phase —
    // so the alt-scroll arrows were already sent and our preventDefault
    // came too late. Registering in the capture phase + stopPropagation
    // means we intercept the wheel before xterm's handler ever sees it.
    this.container.addEventListener(
      "wheel",
      (e) => {
        // Phase 65.O diagnostics (visible in the debug build's devtools):
        // metadata only, never PTY content (Rule #1).
        const onAlt = (() => {
          try {
            return this.term.buffer.active.type === "alternate";
          } catch {
            return false;
          }
        })();
        if (g_winmuxTmuxWheelScroll && this.tmuxScrollProxy && this.sessionId && onAlt) {
          e.preventDefault();
          e.stopPropagation();
          // CSI 1 ; 3 A/B = Alt+Up / Alt+Down (xterm modifier encoding,
          // which tmux parses as M-Up / M-Down).
          const seq = e.deltaY < 0 ? "\x1b[1;3A" : "\x1b[1;3B";
          // Scale to the wheel delta so a fast flick scrolls further.
          // deltaMode 1 = lines, 0 = pixels; normalize both to a small
          // tick count (each proxy key scrolls ~3 lines via the conf).
          const magnitude =
            e.deltaMode === 1 ? Math.abs(e.deltaY) : Math.abs(e.deltaY) / 40;
          const ticks = Math.max(1, Math.min(5, Math.round(magnitude)));
          console.debug(
            `[winmux O] wheel→proxy pane=${this.paneId} deltaY=${e.deltaY} ticks=${ticks} dir=${e.deltaY < 0 ? "up" : "down"}`,
          );
          for (let i = 0; i < ticks; i++) {
            void invoke("pty_write", {
              sessionId: this.sessionId,
              data: seq,
            }).catch((err) => console.error("pty_write (wheel) failed", err));
          }
        } else {
          // Bailed — log WHY so the wheel regression is diagnosable from
          // the devtools console without a rebuild.
          console.debug(
            `[winmux O] wheel passthrough pane=${this.paneId} confEnabled=${g_winmuxTmuxWheelScroll} tmuxProxy=${this.tmuxScrollProxy} hasSession=${!!this.sessionId} onAlt=${onAlt}`,
          );
        }
      },
      { capture: true, passive: false },
    );

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
  }

  /**
   * Attach `dir="auto"` to every row div under `.xterm-rows`, both the
   * ones present now and any that appear later. xterm.js's DOM
   * renderer recycles its row divs as the buffer scrolls, so we use a
   * MutationObserver to keep up. This is what gives us Termius-style
   * "first strong directional char wins per line".
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
    rowsHost.setAttribute("dir", "auto");
    const apply = () => {
      for (const child of Array.from(rowsHost.children)) {
        const el = child as HTMLElement;
        if (el.getAttribute("dir") !== "auto") el.setAttribute("dir", "auto");
      }
    };
    apply();
    const obs = new MutationObserver(apply);
    obs.observe(rowsHost, { childList: true, subtree: false });
    this.dirObserver = obs;
  }

  attach(sessionId: string) {
    this.detach();
    this.sessionId = sessionId;
    this.dataDisposable = this.term.onData((data) => {
      if (this.sessionId)
        invoke("pty_write", { sessionId: this.sessionId, data }).catch((err) =>
          console.error("pty_write failed", err)
        );
    });
    // Phase 25.C: force a pty_resize on attach so tmux gets the
    // current dimensions immediately, even on a reconnect where
    // xterm.js's cols/rows happen to match the previous session.
    this.fitAndResize(true);
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

  writeData(data: string) {
    // Phase 35: queue and coalesce. Merging chunks before the reorder
    // pipeline is also more correct than per-chunk — a chunk boundary
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
    // Phase 62.C (J.1): record (once, metadata only — Rule #1) whether
    // OSC 8 hyperlink sequences (ESC ] 8 ;) actually reach this pane. If
    // the debug.log never shows this line while Claude prints file links,
    // the sequences are being stripped upstream (or Claude isn't emitting
    // them) — not a linkHandler bug.
    if (!this.oscHyperlinkLogged && merged.includes("]8;")) {
      this.oscHyperlinkLogged = true;
      void invoke("diag_log", {
        level: "info",
        msg: `OSC8 hyperlink sequence detected in pane ${this.paneId}`,
      }).catch(() => {});
    }
    // The reorder pipeline keys off the LIVE rtl mode (g_rtlMode), so
    // a settings change takes effect on the very next flush — no
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
    // this terminal — its actions reference this.term.
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
    this.dirObserver?.disconnect();
    this.dirObserver = null;
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
