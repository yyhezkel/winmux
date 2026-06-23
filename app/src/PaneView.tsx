import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Connection, LayoutNode } from "./types";
import { describeConnection, effectiveIdentity } from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { t } from "./i18n";
import { TechText } from "./TechText";

interface ClaudeSessionInfo {
  session_id: string;
  project_path: string;
  jsonl_path: string;
  mtime_unix: number;
  last_user?: string | null;
  last_assistant?: string | null;
}

export type ConnectOpts = {
  password?: string;
  keyPassphrase?: string;
  acceptUnknownHost?: boolean;
  persistent?: boolean;
  mode?: "default" | "tmux" | "plain" | "cmd" | "claude";
  cwdOverride?: string;
  cmd?: string;
  claudeArgs?: string;
  // Phase 23.F: override the auto-derived tmux session name (picker).
  tmuxSession?: string;
};

export type { TmuxSessionInfo } from "./types";

export type PassphrasePending = { paneId: string; keyPath: string; bad?: boolean };
export type HostTrustPending = {
  paneId: string;
  target: string;
  keyType: string;
  fingerprint: string;
  mismatchOld?: string;
};

interface ClaudePickerProps {
  workspaceId: string;
  onClose: () => void;
  // Phase 65 (bug Y): pass the session's original project path so the
  // caller can `cd` there before `claude --resume` — otherwise resume
  // runs from the current cwd (usually $HOME) and Claude can't find the
  // project. Empty string when the session has no recorded path.
  onPick: (sessionId: string, cwd: string) => void;
}

function ClaudeSessionPicker(p: ClaudePickerProps) {
  const [items, setItems] = createSignal<ClaudeSessionInfo[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [err, setErr] = createSignal<string | null>(null);
  onMount(async () => {
    try {
      const list = await invoke<ClaudeSessionInfo[]>("pane_list_claude_sessions", {
        workspaceId: p.workspaceId,
        limit: 30,
      });
      setItems(list);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  });
  const fmtAge = (mt: number) => {
    if (!mt) return "—";
    const sec = Math.max(1, Math.floor(Date.now() / 1000 - mt));
    if (sec < 60) return `${sec}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
    return `${Math.floor(sec / 86400)}d`;
  };
  return (
    <div class="modal-backdrop" onClick={p.onClose}>
      <div class="modal claude-picker" onClick={(e) => e.stopPropagation()}>
        <div class="settings-head">
          <h3>{t("claude_picker.title")}</h3>
          <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
        </div>
        <div class="claude-picker-body">
          <Show when={loading()}><p class="status-line">{t("claude_picker.loading")}</p></Show>
          <Show when={err()}><p class="status-line err">{err()}</p></Show>
          <Show when={!loading() && !err() && items().length === 0}>
            <p class="status-line">{t("claude_picker.empty")}</p>
          </Show>
          <Show when={items().length > 0}>
            <ul class="claude-list">
              {items().map((it) => (
                <li
                  class="claude-row"
                  onClick={() => p.onPick(it.session_id, it.project_path ?? "")}
                  title={it.jsonl_path}
                >
                  <div class="claude-row-head">
                    <code class="claude-id">{it.session_id.slice(0, 8)}</code>
                    <span class="claude-proj">{it.project_path}</span>
                    <span class="claude-age">{fmtAge(it.mtime_unix)}</span>
                  </div>
                  <Show when={it.last_user}>
                    <div class="claude-prev"><b>{t("claude_picker.user_prefix")}</b> {it.last_user}</div>
                  </Show>
                  <Show when={it.last_assistant}>
                    <div class="claude-prev"><b>{t("claude_picker.assistant_prefix")}</b> {it.last_assistant}</div>
                  </Show>
                </li>
              ))}
            </ul>
          </Show>
        </div>
      </div>
    </div>
  );
}

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  // Phase 23.D: the workspace's canonical connection. `isSsh()` falls
  // back to this when the pane itself has no `connection` (FileManager
  // / Browser / ClaudeChat panes, or a fresh Terminal pane). Threaded
  // from App.tsx via LayoutView.
  workspaceConnection?: Connection;
  // Phase 23.I: the workspace name. The pane header falls back to it
  // when the user hasn't set a pane-specific title, replacing the
  // noisy "ssh user@host:port" auto-label.
  workspaceName?: string;
  // Phase 31: the workspace's identity, used to compute the effective
  // pane identity (pane override falls back to these). Drives the
  // pane header border + emoji prefix and the rename dialog's
  // "reset to inherit" hint.
  workspaceColor?: string;
  workspaceEmoji?: string;
  isActive: boolean;
  // Phase 65.T: focus/zoom mode. `isMaximized` = this pane currently
  // fills the workspace area (others run hidden in the background);
  // `backgroundPaneCount` = how many other panes are running while
  // focused (drives the "N in background" header badge).
  isMaximized?: boolean;
  backgroundPaneCount?: number;
  // Phase 26: pane is waiting on a blocking agent permission request
  // (a pending blocking feed item bound to this pane_id). Drives the
  // cmux-style pulsing notification ring around the pane.
  isWaiting?: boolean;
  isConnected: boolean;
  pendingPasswordFor: string | null;
  pendingPassphrase: PassphrasePending | null;
  pendingHostTrust: HostTrustPending | null;
  status: { msg: string; err: boolean } | undefined;
  statusText?: string;
  // Phase 11.A: when this pane is bound to a tmux session, the name. Used
  // to render the "T" badge and to enable "Kill session" in the menu.
  tmuxSession?: string | null;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  ensureTerm: (paneId: string) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: "horizontal" | "vertical") => void;
  onClose: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  // Phase 11.A: hard-kill the remote tmux session. No-op for plain panes.
  onKillSession: (paneId: string) => void;
}

export function PaneView(p: Props) {
  let slotRef!: HTMLDivElement;
  let paneRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  // Phase 49-A: drag-drop into terminal. dropping = visual highlight
  // (border) when a drag enters this pane's bounds; dropMsg = transient
  // status string shown over the pane while an upload is in flight.
  const [dropping, setDropping] = createSignal(false);
  const [dropMsg, setDropMsg] = createSignal<string | null>(null);
  const [pwInput, setPwInput] = createSignal("");
  const [passInput, setPassInput] = createSignal("");
  // Phase 7.A: edit mode for title/annotation.
  const [editingMeta, setEditingMeta] = createSignal(false);
  const [titleDraft, setTitleDraft] = createSignal("");
  const [annotDraft, setAnnotDraft] = createSignal("");
  // Phase 31: identity picker state, mirrors workspace-level picker.
  // `paneColor` / `paneEmoji` hold the pane's own override (None means
  // "inherit from workspace"). `customHex` is the editable field for
  // typing a custom color; reverts on blur if invalid.
  const [paneColor, setPaneColor] = createSignal<string | null>(null);
  const [paneEmoji, setPaneEmoji] = createSignal<string | null>(null);
  const [customHex, setCustomHex] = createSignal("");
  const COLOR_PRESETS = [
    "#1e40af", "#6d28d9", "#16a34a", "#ea580c",
    "#dc2626", "#ca8a04", "#0891b2", "#475569",
  ];
  const EMOJI_PRESETS = ["🟦", "🟣", "🟢", "🟠", "🔴", "🟡", "🔵", "⚪", "⬛"];
  const HEX_RE = /^#[0-9a-fA-F]{6}$/;
  const effective = () =>
    effectiveIdentity(
      { color: paneColor() ?? undefined, emoji: paneEmoji() ?? undefined },
      { color: p.workspaceColor, emoji: p.workspaceEmoji },
    );
  const saveIdentity = async (color: string | null, emoji: string | null) => {
    try {
      await invoke("pane_set_identity", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        color,
        emoji,
      });
      setPaneColor(color);
      setPaneEmoji(emoji);
      setCustomHex(color ?? "");
    } catch (e) {
      console.error("pane_set_identity failed", e);
    }
  };
  const pickColor = (hex: string) => {
    void saveIdentity(hex, paneEmoji());
  };
  const pickEmoji = (g: string) => {
    void saveIdentity(paneColor(), g);
  };
  const onCustomHexBlur = () => {
    const v = customHex().trim();
    if (v === "") {
      setCustomHex(paneColor() ?? "");
      return;
    }
    if (HEX_RE.test(v)) {
      void saveIdentity(v, paneEmoji());
    } else {
      setCustomHex(paneColor() ?? "");
    }
  };
  const onCustomEmojiInput = (v: string) => {
    const trimmed = v.slice(0, 8);
    setPaneEmoji(trimmed === "" ? null : trimmed);
  };
  const onCustomEmojiBlur = () => {
    void saveIdentity(paneColor(), paneEmoji());
  };
  const resetIdentity = () => {
    void saveIdentity(null, null);
  };
  const [showAnnot, setShowAnnot] = createSignal(false);
  // Phase 11.A: dropdown next to the disconnect button.
  const [showDiscMenu, setShowDiscMenu] = createSignal(false);
  // Phase 23.D: workspace dictates connection type. Check pane's own
  // connection first (set on wired Terminal panes), then fall back to
  // the workspace's canonical connection so SSH-only menu items
  // (tmux) show up from FM / Browser / Chat panes too.
  const isSsh = () =>
    (p.pane.connection?.type ?? p.workspaceConnection?.type) === "ssh";
  const isTmux = () => !!p.tmuxSession;
  // Phase 12.B Smart Connect dropdown + extra modals.
  const [showConnectMenu, setShowConnectMenu] = createSignal(false);
  const [smartModal, setSmartModal] = createSignal<null | "cwd" | "cmd" | "claude_args">(null);
  const [smartInput, setSmartInput] = createSignal("");
  const [showClaudePicker, setShowClaudePicker] = createSignal(false);
  // Phase 23.F: tmux session picker state.
  const [tmuxSessions, setTmuxSessions] = createSignal<import("./types").TmuxSessionInfo[] | null>(null);
  const [tmuxPickerLoading, setTmuxPickerLoading] = createSignal(false);
  const [tmuxPickerErr, setTmuxPickerErr] = createSignal<string | null>(null);
  // Phase 23.K: local labels for tmux sessions (session_name → label).
  // Read from %APPDATA%/winmux/tmux-labels.json via tmux_labels_get.
  // The picker shows the label as the primary line when set, raw
  // session name as secondary. Best-effort: any fetch error leaves
  // the map empty and the picker falls back to raw names.
  const [tmuxLabels, setTmuxLabels] = createSignal<Record<string, string>>({});
  const openTmuxPicker = async () => {
    setTmuxSessions([]);
    setTmuxPickerLoading(true);
    setTmuxPickerErr(null);
    try {
      const list = await invoke<import("./types").TmuxSessionInfo[]>("pane_list_tmux_sessions", {
        workspaceId: p.workspaceId,
      });
      setTmuxSessions(list);
    } catch (e) {
      setTmuxPickerErr(String(e));
      setTmuxSessions([]);
    } finally {
      setTmuxPickerLoading(false);
    }
    // Phase 23.K: fetch labels in parallel. Failures are silent —
    // labels are a UI sugar, not load-bearing.
    try {
      const labels = await invoke<Record<string, string>>("tmux_labels_get", {
        workspaceId: p.workspaceId,
      });
      setTmuxLabels(labels ?? {});
    } catch {
      setTmuxLabels({});
    }
  };
  const closeTmuxPicker = () => {
    setTmuxSessions(null);
    setTmuxPickerErr(null);
    setTmuxLabels({});
  };
  // Phase 23.I: removed renameErrors + renameTmuxSession. Pane title
  // is now the canonical tmux session name — edit the title via the
  // pane header's ✎ button instead. Backend pane_set_title auto-runs
  // tmux rename-session over the existing SSH handle.
  const closeConnectMenu = () => setShowConnectMenu(false);
  const submitSmartModal = () => {
    const m = smartModal();
    const v = smartInput();
    setSmartModal(null);
    setSmartInput("");
    if (m === "cwd") p.onConnect(p.pane.pane_id, { cwdOverride: v });
    if (m === "cmd") p.onConnect(p.pane.pane_id, { mode: "cmd", cmd: v });
    if (m === "claude_args") p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: v });
  };

  // Phase 65 (bug AA): "Open in directory" folder picker. Replaces the
  // bare text input — browse the remote tree (SFTP dir-list) with
  // drill-down + a recent-dirs shortcut list (per workspace,
  // localStorage). Local (non-SSH) panes keep the text-input fallback,
  // since file_list_remote needs an SSH session.
  const [dirPicker, setDirPicker] = createSignal<{
    path: string;
    dirs: string[];
    loading: boolean;
    error: string | null;
  } | null>(null);
  const recentDirsKey = () => `winmux.recent-dirs.${p.workspaceId}`;
  const loadRecentDirs = (): string[] => {
    try {
      const raw = localStorage.getItem(recentDirsKey());
      const parsed: unknown = raw ? JSON.parse(raw) : [];
      return Array.isArray(parsed)
        ? parsed.filter((x): x is string => typeof x === "string")
        : [];
    } catch {
      return [];
    }
  };
  const [recentDirs, setRecentDirs] = createSignal<string[]>([]);
  const pushRecentDir = (dir: string) => {
    const next = [dir, ...loadRecentDirs().filter((d) => d !== dir)].slice(0, 8);
    try {
      localStorage.setItem(recentDirsKey(), JSON.stringify(next));
    } catch {
      // quota / private mode — recents are best-effort
    }
    setRecentDirs(next);
  };
  const navigateDirPicker = async (path: string) => {
    setDirPicker({ path, dirs: [], loading: true, error: null });
    try {
      const list = await invoke<{ name: string; is_dir: boolean }[]>(
        "file_list_remote",
        { workspaceId: p.workspaceId, path, showHidden: false },
      );
      const dirs = list
        .filter((e) => e.is_dir)
        .map((e) => e.name)
        .sort((a, b) => a.localeCompare(b));
      setDirPicker({ path, dirs, loading: false, error: null });
    } catch (e) {
      setDirPicker({ path, dirs: [], loading: false, error: String(e) });
    }
  };
  const openDirPicker = async () => {
    setRecentDirs(loadRecentDirs());
    if (!isSsh()) {
      // Local pane: no SFTP — fall back to the text input.
      setSmartInput("");
      setSmartModal("cwd");
      return;
    }
    let start = "/";
    try {
      start = (await invoke<string>("file_home_remote", {
        workspaceId: p.workspaceId,
      })) || "/";
    } catch {
      start = "/";
    }
    void navigateDirPicker(start);
  };
  const dirPickerParent = (path: string): string => {
    const trimmed = path.replace(/\/+$/, "");
    const idx = trimmed.lastIndexOf("/");
    if (idx <= 0) return "/";
    return trimmed.slice(0, idx);
  };
  const dirPickerJoin = (path: string, name: string): string =>
    path === "/" ? `/${name}` : `${path.replace(/\/+$/, "")}/${name}`;
  const chooseDir = (dir: string) => {
    pushRecentDir(dir);
    setDirPicker(null);
    p.onConnect(p.pane.pane_id, { cwdOverride: dir });
  };
  const openMeta = () => {
    setTitleDraft(p.pane.title ?? "");
    setAnnotDraft(p.pane.annotation ?? "");
    // Phase 31: hydrate identity from the pane prop (the source of
    // truth between dialog opens). Falls through to None when the
    // pane has no override and is inheriting from the workspace.
    setPaneColor(p.pane.color ?? null);
    setPaneEmoji(p.pane.emoji ?? null);
    setCustomHex(p.pane.color ?? "");
    setEditingMeta(true);
  };
  const saveMeta = () => {
    const newTitle = titleDraft();
    const newAnnot = annotDraft();
    if ((p.pane.title ?? "") !== newTitle)
      p.onSetTitle(p.pane.pane_id, newTitle);
    if ((p.pane.annotation ?? "") !== newAnnot)
      p.onSetAnnotation(p.pane.pane_id, newAnnot);
    setEditingMeta(false);
  };

  // Phase 35 (#1.3): command-palette "pane.rename" dispatches this
  // window event with the target pane id; the matching pane opens its
  // title/annotation editor. Lightweight cross-component trigger that
  // avoids prop-drilling a rename request down from App.
  const onRenameRequest = (e: Event) => {
    const detail = (e as CustomEvent).detail;
    if (detail === p.pane.pane_id) openMeta();
  };

  // Phase 49-A: POSIX single-quote escape for paths typed into the
  // shell. `'foo bar'` is literal; an embedded ' is closed, escaped,
  // and re-opened: foo'bar → 'foo'\''bar'. Safe for any byte sequence.
  const posixQuote = (s: string): string =>
    `'${s.replace(/'/g, `'\\''`)}'`;

  // Effective connection for this pane — pane override beats workspace
  // default. Used to route drops to SFTP (SSH) vs. local-path passthrough.
  const effectiveConn = (): Connection | null =>
    p.pane.connection ?? p.workspaceConnection ?? null;
  const isSshPane = () => effectiveConn()?.type === "ssh";

  // Phase 49-A: turn one dropped file path into a string suitable for
  // pty_write. SSH workspaces uploaded via SFTP; the returned remote
  // path is what gets typed. Local panes type the host path verbatim.
  const handleOneDrop = async (hostPath: string): Promise<string | null> => {
    const basename =
      hostPath.split(/[\\/]/).filter(Boolean).pop() || "dropped";
    if (!isSshPane()) {
      return hostPath;
    }
    try {
      setDropMsg(t("pane.drop.uploading", { name: basename }));
      const remote = await invoke<string>("pane_upload_dropped", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        localPath: hostPath,
        fileName: basename,
      });
      setDropMsg(t("pane.drop.uploaded", { name: basename }));
      return remote;
    } catch (e) {
      console.error("pane_upload_dropped failed", e);
      setDropMsg(t("pane.drop.failed", { name: basename, err: String(e) }));
      return null;
    }
  };

  // Phase 49-A: hit-test helper; returns true if (x, y) — in CSS px —
  // sits inside the pane's bounding box. Tauri drag positions arrive
  // in physical px; caller divides by DPR before invoking.
  const pointInPane = (x: number, y: number): boolean => {
    if (!paneRef) return false;
    const r = paneRef.getBoundingClientRect();
    return x >= r.left && x < r.right && y >= r.top && y < r.bottom;
  };

  const writeToPty = (s: string) => {
    if (!ti?.sessionId) return;
    void invoke("pty_write", { sessionId: ti.sessionId, data: s }).catch(
      (e) => console.error("pty_write failed", e),
    );
  };

  onMount(() => {
    ti = p.ensureTerm(p.pane.pane_id);
    if (ti.container.parentElement !== slotRef) {
      slotRef.appendChild(ti.container);
    }
    ti.container.style.display = "block";
    requestAnimationFrame(() => ti?.fitAndResize());
    window.addEventListener("winmux:pane-rename", onRenameRequest);

    // Phase 49-A: subscribe to the window-wide drag-drop event. Each
    // PaneView registers its own listener and hit-tests against its own
    // bounding rect, so multi-pane layouts route the drop to whichever
    // pane the cursor was over. File-manager panes register their own
    // listener at a different on-screen location, so there's no double
    // claim. The webview consumes file drops at the OS level, so this
    // handler is the only path for OS-file drops; the HTML5 ondrop on
    // the pane div picks up text/URL drags from the browser.
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload as
            | { type: "enter" | "over"; position: { x: number; y: number } }
            | { type: "drop"; paths: string[]; position: { x: number; y: number } }
            | { type: "leave" };
          if (payload.type === "leave") {
            setDropping(false);
            return;
          }
          const dpr = window.devicePixelRatio || 1;
          const x = payload.position.x / dpr;
          const y = payload.position.y / dpr;
          const inside = pointInPane(x, y);
          if (payload.type === "enter" || payload.type === "over") {
            setDropping(inside);
            return;
          }
          setDropping(false);
          if (payload.type !== "drop" || !inside) return;
          const paths = payload.paths || [];
          if (paths.length === 0) return;
          void (async () => {
            for (const hostPath of paths) {
              const typed = await handleOneDrop(hostPath);
              if (typed) writeToPty(posixQuote(typed) + " ");
            }
            // Clear the toast after a short grace so the user sees it.
            setTimeout(() => setDropMsg(null), 1800);
          })();
        });
      } catch (e) {
        console.warn("pane: onDragDropEvent failed:", e);
      }
    })();

    // Cleanup for the async-assigned unlisten.
    onCleanup(() => {
      try { unlisten?.(); } catch {}
    });
  });

  onCleanup(() => {
    window.removeEventListener("winmux:pane-rename", onRenameRequest);
    if (ti && ti.container.parentElement === slotRef) {
      ti.container.parentElement.removeChild(ti.container);
    }
  });

  // Phase 49-A: HTML5 drop for non-file drags (URLs / text dragged
  // from browser tabs). Tauri's onDragDropEvent only fires for OS-level
  // file drops, so URLs need this fallback. URI-list takes priority,
  // then plain text. Same rule: type the string + SPACE.
  const onHtml5Drop = (e: DragEvent) => {
    if (!e.dataTransfer) return;
    // If files are present, Tauri's handler already routed them; bail.
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) return;
    const uri = e.dataTransfer.getData("text/uri-list").trim();
    const txt = uri || e.dataTransfer.getData("text/plain").trim();
    if (!txt) return;
    e.preventDefault();
    setDropping(false);
    writeToPty(posixQuote(txt) + " ");
  };
  const onHtml5DragOver = (e: DragEvent) => {
    // Allow drop. Don't preventDefault for file drops or Tauri's
    // OS-level handler won't see them.
    if (e.dataTransfer?.types?.includes("text/uri-list") ||
        e.dataTransfer?.types?.includes("text/plain")) {
      e.preventDefault();
    }
  };

  const passphraseHere = () =>
    p.pendingPassphrase && p.pendingPassphrase.paneId === p.pane.pane_id
      ? p.pendingPassphrase
      : null;

  const hostTrustHere = () =>
    p.pendingHostTrust && p.pendingHostTrust.paneId === p.pane.pane_id
      ? p.pendingHostTrust
      : null;

  // Phase 31: live effective identity — recomputed when pane props
  // change OR when the user picks something in the open editor.
  const liveEffective = () => {
    const e = effective();
    return {
      color: p.pane.color ?? e.color,
      emoji: p.pane.emoji ?? e.emoji,
    };
  };
  return (
    <div
      ref={(el) => (paneRef = el)}
      class={`pane ${p.isActive ? "active" : ""} ${p.isWaiting ? "waiting" : ""} ${dropping() ? "drop-target" : ""}`}
      data-has-color={liveEffective().color ? "true" : "false"}
      style={liveEffective().color ? `--pane-color: ${liveEffective().color}` : undefined}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
      onDrop={onHtml5Drop}
      onDragOver={onHtml5DragOver}
      onDblClick={(e) => {
        // Phase 55-A: maximize toggle on content double-click. Skip
        // when the click landed inside the xterm canvas — xterm's own
        // double-click handler uses that for word-selection. Skip the
        // header too (which has its own rename / connect actions
        // bound to clicks).
        const target = e.target as HTMLElement;
        if (target.closest(".xterm")) return;
        if (target.closest(".pane-header")) return;
        if (target.closest(".pane-drop-toast")) return;
        window.dispatchEvent(
          new CustomEvent("winmux:pane-maximize", {
            detail: { paneId: p.pane.pane_id },
          })
        );
      }}
    >
      <Show when={dropMsg()}>
        <div class="pane-drop-toast">{dropMsg()}</div>
      </Show>
      <div class="pane-header">
        {/* Phase 23.I: header fallback chain — user-set pane.title
            beats workspace name beats the raw SSH URL. The old
            describeConnection() output (e.g. "ssh runner@1.2.3.4:22")
            was noisy and only useful for debugging.
            Phase 31: prepend the effective emoji glyph when set. */}
        <span
          class="pane-conn"
          title={
            p.pane.connection
              ? describeConnection(p.pane.connection)
              : p.workspaceConnection
                ? describeConnection(p.workspaceConnection)
                : undefined
          }
        >
          <Show when={liveEffective().emoji}>
            <span class="pane-emoji">{liveEffective().emoji}</span>{" "}
          </Show>
          <TechText text={
            p.pane.title
              ?? p.workspaceName
              ?? (p.pane.connection
                ? describeConnection(p.pane.connection)
                : p.workspaceConnection
                  ? describeConnection(p.workspaceConnection)
                  : "—")
          } />
        </span>
        <Show when={p.pane.annotation}>
          <button
            class="pane-btn"
            title={t("pane.tooltip.show_annotation")}
            onClick={(e) => {
              e.stopPropagation();
              setShowAnnot(!showAnnot());
            }}
          >
            ⓘ
          </button>
        </Show>
        <Show when={p.statusText}>
          <span class="pane-status-text">{p.statusText}</span>
        </Show>
        <button
          class="pane-btn"
          title={t("pane.tooltip.edit_meta")}
          onClick={(e) => {
            e.stopPropagation();
            openMeta();
          }}
        >
          ✎
        </button>
        <Show when={isTmux()}>
          <span
            class="pane-tmux-badge"
            title={t("pane.tooltip.tmux_badge")}
          >
            T
          </span>
        </Show>
        <Show when={p.isConnected}>
          <div class="pane-disc-wrap">
            <button
              class="pane-btn"
              title={isTmux() ? t("pane.tooltip.detach") : t("pane.tooltip.disconnect")}
              onClick={() => p.onDisconnect(p.pane.pane_id)}
            >
              ⏻
            </button>
            <button
              class="pane-btn pane-disc-caret"
              title={t("pane.tooltip.kill_session")}
              onClick={(e) => {
                e.stopPropagation();
                setShowDiscMenu(!showDiscMenu());
              }}
            >
              ▾
            </button>
            <Show when={showDiscMenu()}>
              <div
                class="pane-disc-menu"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowDiscMenu(false);
                }}
              >
                <button onClick={() => p.onDisconnect(p.pane.pane_id)}>
                  {isTmux() ? t("common.detach") : t("common.disconnect")}
                </button>
                <Show when={isTmux()}>
                  <button class="danger" onClick={() => p.onKillSession(p.pane.pane_id)}>
                    {t("common.kill_session")}
                  </button>
                </Show>
              </div>
            </Show>
          </div>
        </Show>
        {/* Phase 52 (BiDi 33B): per-pane opt-in PTY-stream bidi filter.
            Default off; click toggles via pane_set_smart_bidi. ⇆ glyph
            picked from "left right arrow" since the filter swaps RTL
            isolates around Latin runs. */}
        <button
          class={`pane-btn ${p.pane.smart_bidi === true ? "active" : ""}`}
          title={t(p.pane.smart_bidi === true ? "pane.smartBidi.on" : "pane.smartBidi.off") + " — " + t("pane.smartBidi.hint")}
          onClick={(e) => {
            e.stopPropagation();
            const next = !(p.pane.smart_bidi === true);
            void invoke("pane_set_smart_bidi", {
              workspaceId: p.workspaceId,
              paneId: p.pane.pane_id,
              enabled: next,
            }).catch((err) => console.error("pane_set_smart_bidi failed", err));
          }}
        >
          ⇆
        </button>
        {/* Phase 65.T: focus/zoom badge + toggle. The badge shows how
            many panes keep running in the background while this one is
            focused. The button dispatches the same winmux:pane-maximize
            event as double-click / Ctrl+Shift+M. */}
        <Show when={p.isMaximized && (p.backgroundPaneCount ?? 0) > 0}>
          <span
            class="pane-bg-badge"
            title={t("pane.tooltip.background_panes", {
              count: String(p.backgroundPaneCount ?? 0),
            })}
          >
            ⛶ {p.backgroundPaneCount}
          </span>
        </Show>
        <button
          class={`pane-btn ${p.isMaximized ? "active" : ""}`}
          title={p.isMaximized ? t("pane.tooltip.restore") : t("pane.tooltip.focus")}
          onClick={(e) => {
            e.stopPropagation();
            window.dispatchEvent(
              new CustomEvent("winmux:pane-maximize", {
                detail: { paneId: p.pane.pane_id },
              }),
            );
          }}
        >
          {p.isMaximized ? "⤡" : "⛶"}
        </button>
        <button class="pane-btn" title="Split right (Ctrl+Shift+D)" onClick={() => p.onSplit(p.pane.pane_id, "horizontal")}>↔</button>
        <button class="pane-btn" title="Split down (Ctrl+Shift+E)" onClick={() => p.onSplit(p.pane.pane_id, "vertical")}>↕</button>
        <button class="pane-btn pane-close" title={t("pane.tooltip.close")} onClick={() => p.onClose(p.pane.pane_id)}>×</button>
      </div>
      <Show when={editingMeta()}>
        <div class="pane-meta-editor" onMouseDown={(e) => e.stopPropagation()}>
          <input
            class="pane-meta-title"
            placeholder="title (e.g. trying to find the X bug)"
            maxlength="200"
            value={titleDraft()}
            onInput={(e) => setTitleDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          <textarea
            class="pane-meta-annot"
            placeholder="annotation (longer free text — context, intent, links)"
            rows="3"
            value={annotDraft()}
            onInput={(e) => setAnnotDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                saveMeta();
              } else if (e.key === "Escape") {
                setEditingMeta(false);
              }
            }}
          />
          {/* Phase 31: identity picker. Same UX as the workspace
              picker (Phase 30), reusing the `ws-identity-*` CSS classes
              and i18n keys. Each click instant-saves via
              pane_set_identity, so the user can preview the border
              color change live behind the open dialog. Reset clears the
              pane's own values → falls back to workspace inheritance. */}
          <div class="ws-identity-block">
            <div class="ws-identity-label">{t("ws.identity.color")}</div>
            <div class="ws-identity-row">
              <For each={COLOR_PRESETS}>
                {(c) => (
                  <button
                    type="button"
                    class={`ws-identity-swatch ${paneColor() === c ? "selected" : ""}`}
                    style={{ background: c }}
                    title={c}
                    onClick={(e) => {
                      e.stopPropagation();
                      pickColor(c);
                    }}
                  />
                )}
              </For>
              <input
                type="text"
                class="ws-identity-hex"
                value={customHex()}
                placeholder={t("ws.identity.customColor")}
                spellcheck={false}
                onInput={(e) => setCustomHex(e.currentTarget.value)}
                onBlur={onCustomHexBlur}
              />
            </div>
            <div class="ws-identity-label" style="margin-top: 8px">{t("ws.identity.emoji")}</div>
            <div class="ws-identity-row">
              <For each={EMOJI_PRESETS}>
                {(g) => (
                  <button
                    type="button"
                    class={`ws-identity-emoji-btn ${paneEmoji() === g ? "selected" : ""}`}
                    title={g}
                    onClick={(e) => {
                      e.stopPropagation();
                      pickEmoji(g);
                    }}
                  >
                    {g}
                  </button>
                )}
              </For>
              <input
                type="text"
                class="ws-identity-emoji-custom"
                value={paneEmoji() ?? ""}
                placeholder={t("ws.identity.customEmoji")}
                maxlength={8}
                onInput={(e) => onCustomEmojiInput(e.currentTarget.value)}
                onBlur={onCustomEmojiBlur}
              />
              <button
                type="button"
                class="ws-identity-reset"
                onClick={resetIdentity}
              >
                {t("ws.identity.reset")}
              </button>
            </div>
          </div>
          <div class="pane-meta-actions">
            <button class="primary" onClick={saveMeta}>
              Save
            </button>
            <button onClick={() => setEditingMeta(false)}>Cancel</button>
            <span class="pane-meta-hint">
              Enter to save title; Ctrl+Enter to save from annotation; Esc to cancel
            </span>
          </div>
        </div>
      </Show>
      <Show when={showAnnot() && p.pane.annotation}>
        <div class="pane-annotation-bar">{p.pane.annotation}</div>
      </Show>
      <div class="pane-body">
        <Show when={!p.isConnected}>
          <div class="pane-connect">
            {/* Host-trust dialog (unknown host or mismatch) — highest priority */}
            <Show when={hostTrustHere()}>
              <div class={`host-trust ${hostTrustHere()!.mismatchOld ? "danger" : ""}`}>
                <Show
                  when={hostTrustHere()!.mismatchOld}
                  fallback={
                    <h3>First connect to {hostTrustHere()!.target}</h3>
                  }
                >
                  <h3>⚠ HOST KEY CHANGED for {hostTrustHere()!.target}</h3>
                </Show>
                <Show when={hostTrustHere()!.mismatchOld}>
                  <p class="warn">
                    The server's host key is different from the one we trusted before.
                    This may indicate a man-in-the-middle attack — or the server was rekeyed.
                  </p>
                  <p>
                    <span class="label">Old fingerprint:</span>{" "}
                    <code>{hostTrustHere()!.mismatchOld}</code>
                  </p>
                </Show>
                <p>
                  <span class="label">{hostTrustHere()!.keyType} fingerprint:</span>{" "}
                  <code>{hostTrustHere()!.fingerprint}</code>
                </p>
                <div class="trust-buttons">
                  <button
                    class="primary"
                    onClick={() =>
                      p.onConnect(p.pane.pane_id, { acceptUnknownHost: true })
                    }
                  >
                    {hostTrustHere()!.mismatchOld ? "Replace and continue" : "Trust and continue"}
                  </button>
                  <button onClick={() => p.onConnect(p.pane.pane_id, {})}>Cancel</button>
                </div>
              </div>
            </Show>

            {/* Passphrase prompt for encrypted local key */}
            <Show when={!hostTrustHere() && passphraseHere()}>
              <div class="pw-row">
                <span class="pass-hint">
                  Passphrase for {passphraseHere()!.keyPath}
                  {passphraseHere()!.bad ? " (wrong, try again)" : ""}:
                </span>
                <input
                  type="password"
                  placeholder="key passphrase"
                  autofocus
                  value={passInput()}
                  onInput={(e) => setPassInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = passInput();
                      setPassInput("");
                      p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = passInput();
                    setPassInput("");
                    p.onConnect(p.pane.pane_id, { keyPassphrase: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Password prompt (server auth) */}
            <Show when={!hostTrustHere() && !passphraseHere() && p.pendingPasswordFor === p.pane.pane_id}>
              <div class="pw-row">
                <input
                  type="password"
                  placeholder="password"
                  autofocus
                  value={pwInput()}
                  onInput={(e) => setPwInput(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      const v = pwInput();
                      setPwInput("");
                      p.onConnect(p.pane.pane_id, { password: v });
                    }
                  }}
                />
                <button
                  class="primary"
                  onClick={() => {
                    const v = pwInput();
                    setPwInput("");
                    p.onConnect(p.pane.pane_id, { password: v });
                  }}
                >
                  Connect
                </button>
              </div>
            </Show>

            {/* Default Connect button when no special prompt */}
            <Show
              when={
                !hostTrustHere() &&
                !passphraseHere() &&
                p.pendingPasswordFor !== p.pane.pane_id
              }
            >
              <div class="connect-buttons">
                {/* Phase 12.B Smart Connect — split button. Main click =
                    plain Connect. Caret opens the menu with tmux / plain /
                    cwd / cmd / claude options. */}
                <div class="connect-split">
                  <button class="primary big" onClick={() => p.onConnect(p.pane.pane_id, {})}>
                    {t("common.connect")}
                  </button>
                  <button
                    class="primary big connect-caret"
                    title={t("pane.tooltip.more_connect_options")}
                    onClick={(e) => {
                      e.stopPropagation();
                      setShowConnectMenu(!showConnectMenu());
                    }}
                  >
                    ▾
                  </button>
                  <Show when={showConnectMenu()}>
                    <div
                      class="connect-menu"
                      onClick={(e) => e.stopPropagation()}
                    >
                      <Show when={isSsh()}>
                        {/* Phase 23.F: open picker instead of attaching
                             to the auto-named session. */}
                        <button onClick={() => { closeConnectMenu(); openTmuxPicker(); }}>
                          {t("common.connect_tmux")}
                        </button>
                      </Show>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "plain" }); }}>
                        {t("connect.plainShell")}
                      </button>
                      <hr />
                      <button onClick={() => { closeConnectMenu(); openDirPicker(); }}>
                        {t("connect.openDir")}
                      </button>
                      <button onClick={() => { closeConnectMenu(); setSmartInput(""); setSmartModal("cmd"); }}>
                        {t("connect.runCmd")}
                      </button>
                      <hr />
                      {/* Phase 61: Claude launchers are no longer SSH-only —
                          the backend injects shell-appropriate syntax for
                          local PowerShell / Cmd panes too. */}
                      <div class="connect-menu-section">{t("connect.runClaude")}</div>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude" }); }}>
                        claude
                      </button>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--continue" }); }}>
                        claude --continue
                      </button>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--resume" }); }}>
                        claude --resume
                      </button>
                      <button onClick={() => { closeConnectMenu(); p.onConnect(p.pane.pane_id, { mode: "claude", claudeArgs: "--dangerously-skip-permissions" }); }}>
                        claude --dangerously-skip-permissions
                      </button>
                      <button onClick={() => { closeConnectMenu(); setShowClaudePicker(true); }}>
                        {t("connect.resumeList")}
                      </button>
                    </div>
                  </Show>
                </div>
              </div>
            </Show>

            <Show when={p.status}>
              <p class={p.status!.err ? "status-line err" : "status-line"}>
                {p.status!.msg}
              </p>
            </Show>
          </div>
        </Show>
        <div ref={slotRef!} class="pane-terminal-slot" />
      </div>

      {/* Phase 12.B: smart-connect prompt for cwd / cmd / claude args */}
      <Show when={smartModal()}>
        <div class="modal-backdrop" onClick={() => setSmartModal(null)}>
          <div class="modal smart-prompt" onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
            <h3>
              {smartModal() === "cwd" && t("connect.modal.openDir")}
              {smartModal() === "cmd" && t("connect.modal.runCmd")}
              {smartModal() === "claude_args" && t("connect.modal.claudeArgs")}
            </h3>
            <input
              class="pane-meta-title"
              autofocus
              placeholder={
                smartModal() === "cwd"
                  ? "/home/yossi/projects/foo"
                  : smartModal() === "cmd"
                    ? "npm run dev"
                    : "--resume"
              }
              value={smartInput()}
              onInput={(e) => setSmartInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitSmartModal();
                else if (e.key === "Escape") setSmartModal(null);
              }}
            />
            <div class="modal-buttons">
              <button onClick={() => setSmartModal(null)}>{t("common.cancel")}</button>
              <button class="primary" onClick={submitSmartModal}>{t("common.connect")}</button>
            </div>
          </div>
        </div>
      </Show>

      {/* Phase 65 (bug AA): remote folder picker for "Open in directory". */}
      <Show when={dirPicker()}>
        <div class="modal-backdrop" onClick={() => setDirPicker(null)}>
          <div
            class="modal claude-picker"
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <div class="settings-head">
              <h3>{t("connect.dirPicker.title")}</h3>
              <button class="feed-x" title={t("common.close")} onClick={() => setDirPicker(null)}>×</button>
            </div>
            <div class="dir-picker-path" title={dirPicker()!.path}>{dirPicker()!.path}</div>
            <Show when={recentDirs().length > 0}>
              <div class="dir-picker-recent">
                <div class="dir-picker-recent-label">{t("connect.dirPicker.recent")}</div>
                <For each={recentDirs()}>
                  {(d) => (
                    <button class="dir-picker-recent-row" title={d} onClick={() => chooseDir(d)}>
                      🕘 {d}
                    </button>
                  )}
                </For>
              </div>
            </Show>
            <div class="claude-picker-body">
              <Show when={dirPicker()!.loading}>
                <p class="status-line">{t("connect.dirPicker.loading")}</p>
              </Show>
              <Show when={dirPicker()!.error}>
                <p class="status-line err">⚠ {dirPicker()!.error}</p>
              </Show>
              <ul class="dir-picker-list">
                <Show when={dirPicker()!.path !== "/"}>
                  <li class="dir-picker-row up" onClick={() => void navigateDirPicker(dirPickerParent(dirPicker()!.path))}>
                    📁 ..
                  </li>
                </Show>
                <For each={dirPicker()!.dirs}>
                  {(name) => (
                    <li
                      class="dir-picker-row"
                      onClick={() => void navigateDirPicker(dirPickerJoin(dirPicker()!.path, name))}
                    >
                      📁 {name}
                    </li>
                  )}
                </For>
                <Show when={!dirPicker()!.loading && dirPicker()!.dirs.length === 0 && !dirPicker()!.error}>
                  <li class="dir-picker-empty">{t("connect.dirPicker.empty")}</li>
                </Show>
              </ul>
            </div>
            <div class="modal-buttons">
              <button onClick={() => setDirPicker(null)}>{t("common.cancel")}</button>
              <button class="primary" onClick={() => chooseDir(dirPicker()!.path)}>
                {t("connect.dirPicker.useThis")}
              </button>
            </div>
          </div>
        </div>
      </Show>

      {/* Phase 12.B: Claude session browser */}
      <Show when={showClaudePicker()}>
        <ClaudeSessionPicker
          workspaceId={p.workspaceId}
          onClose={() => setShowClaudePicker(false)}
          onPick={(sessionId, cwd) => {
            setShowClaudePicker(false);
            // Phase 65 (bug Y): cd to the session's original project dir
            // first (backend turns cwdOverride into `cd <dir> && exec
            // claude …`), so resume runs where the session was created.
            p.onConnect(p.pane.pane_id, {
              mode: "claude",
              claudeArgs: `--resume ${sessionId}`,
              ...(cwd ? { cwdOverride: cwd } : {}),
            });
          }}
        />
      </Show>

      {/* Phase 23.F: tmux session picker. */}
      <Show when={tmuxSessions() !== null}>
        <div class="modal-backdrop" onClick={closeTmuxPicker}>
          <div class="modal claude-picker" onClick={(e) => e.stopPropagation()}>
            <div class="settings-head">
              <h3>{t("tmux_picker.title")}</h3>
              <button class="feed-x" title={t("common.close")} onClick={closeTmuxPicker}>×</button>
            </div>
            <div class="claude-picker-body">
              <Show when={tmuxPickerLoading()}>
                <p class="status-line">{t("tmux_picker.loading")}</p>
              </Show>
              <Show when={tmuxPickerErr()}>
                <p class="status-line err">⚠ {tmuxPickerErr()}</p>
              </Show>
              <ul class="claude-list">
                <li
                  class="claude-row"
                  onClick={() => {
                    closeTmuxPicker();
                    // Phase 23.K: generate a fresh unique tmux name
                    // client-side so `tmux new-session -A` doesn't
                    // silently attach to whatever existing session
                    // happened to be derived from pane.title / pane_id.
                    const freshName = `new_${Date.now().toString(36)}`;
                    p.onConnect(p.pane.pane_id, {
                      mode: "tmux",
                      tmuxSession: freshName,
                    });
                  }}
                >
                  <div class="claude-row-head">
                    <code class="claude-id">🆕</code>
                    <span class="claude-proj"><b>{t("tmux_picker.new_session")}</b></span>
                    <span class="claude-age">{t("tmux_picker.pane_id_target")}</span>
                  </div>
                  <div class="claude-prev">{t("tmux_picker.new_session_hint_v2")}</div>
                </li>
                {(tmuxSessions() ?? []).map((s) => {
                  const ageOf = (epoch: number) => {
                    if (!epoch) return "—";
                    const sec = Math.max(1, Math.floor(Date.now() / 1000 - epoch));
                    if (sec < 60) return `${sec}s`;
                    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
                    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
                    return `${Math.floor(sec / 86400)}d`;
                  };
                  const winLabel = s.windows === 1
                    ? t("tmux_picker.window", { n: String(s.windows) })
                    : t("tmux_picker.windows", { n: String(s.windows) });
                  // Phase 23.K: prefer the user's local label over the
                  // raw tmux session name. The raw name still shows as
                  // small secondary text so power users can map back
                  // to `tmux ls` output.
                  const label = tmuxLabels()[s.name];
                  return (
                    <li
                      class="claude-row"
                      onClick={() => {
                        closeTmuxPicker();
                        p.onConnect(p.pane.pane_id, {
                          mode: "tmux",
                          tmuxSession: s.name,
                        });
                      }}
                      title={`Created ${ageOf(s.created)} ago${s.last_attached ? `, last attached ${ageOf(s.last_attached)} ago` : ""}`}
                    >
                      <div class="claude-row-head">
                        <code class="claude-id">
                          {label ? label.slice(0, 24) : s.name.slice(0, 14)}
                        </code>
                        <span class="claude-proj">
                          {winLabel}
                          {s.attached ? ` · ${t("tmux_picker.attached")}` : ""}
                        </span>
                        <span class="claude-age">{ageOf(s.last_attached || s.created)}</span>
                      </div>
                      <Show when={label}>
                        <div class="claude-prev">
                          {t("tmux_picker.label_secondary", { name: s.name })}
                        </div>
                      </Show>
                    </li>
                  );
                })}
              </ul>
              <Show when={!tmuxPickerLoading() && (tmuxSessions()?.length ?? 0) === 0 && !tmuxPickerErr()}>
                <p class="status-line">{t("tmux_picker.empty")}</p>
              </Show>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
