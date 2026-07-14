import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Connection, LayoutNode, TmuxSessionInfo } from "./types";
import { describeConnection, effectiveIdentity, isRemoteConn, isRemoteEffective } from "./types";
import type { TerminalInstance } from "./terminalInstance";
import { t } from "./i18n";
import { TechText } from "./TechText";
import {
  paneDragStore,
  startPaneDrag,
} from "./paneDrag";
import {
  IconPencil,
  IconPower,
  IconChevronDown,
  IconArrowLeftRight,
  IconMaximize,
  IconMinimize,
  IconColumns,
  IconRows,
  IconExternalLink,
  IconClose,
  IconWarning,
  IconTerminal,
  IconRefresh,
  IconClock,
  IconFolder,
  IconInfo,
} from "./icons";

interface ClaudeSessionInfo {
  session_id: string;
  project_path: string;
  jsonl_path: string;
  mtime_unix: number;
  last_user?: string | null;
  last_assistant?: string | null;
  is_subagent: boolean;
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
  // cmux-A A1: an OSC 9/99/777 terminal notification arrived for this
  // pane and it hasn't been focused since. Drives the amber activity
  // pulse (distinct from the waiting/blocking ring). Setting-gated in
  // the parent so we can render it as a plain boolean here.
  isNotified?: boolean;
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
  // Unshipped-fivefer (#4): pop this pane's terminal into its own window.
  onPopOut: (paneId: string) => void;
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
  const isSsh = () => isRemoteEffective(p.pane, p.workspaceConnection);
  const isTmux = () => !!p.tmuxSession;
  // Phase 12.B Smart Connect — the "open in directory" text-input fallback
  // (local panes) still uses this small prompt.
  const [smartModal, setSmartModal] = createSignal<null | "cwd" | "cmd" | "claude_args">(null);
  const [smartInput, setSmartInput] = createSignal("");
  // v0.4.4 (Task 2): unified "new connection" picker. One modal to choose a
  // directory AND a launch command together, then connect — instead of the
  // à-la-carte menu that only ever set one at a time. Connect-time only,
  // nothing persisted. The backend build_smart_connect_script already
  // combines cwd_override + claude/cmd ("cd … && claude"), so this is pure
  // UI. Attaching to an existing tmux session stays a direct action (no
  // picker) via the separate tmux path below.
  type NcCmd =
    | "plain"
    | "claude"
    | "claude-continue"
    | "claude-resume"
    | "claude-skip"
    | "from-list"
    | "custom";
  // v0.4.4-beta.2: connection type is the FIRST step — "regular" = SSH → bare
  // shell; "tmux" = SSH → tmux new-session. Maps to the `persistent` flag
  // (regular=false, tmux=true); the command choice is orthogonal.
  type NcType = "regular" | "tmux";
  const [newConnModal, setNewConnModal] = createSignal(false);
  // v0.4.4-beta.2: the modal is a single shell with swappable views; the
  // header/footer/dimensions stay constant, only the body changes.
  type NcView = "form" | "browse";
  const [ncView, setNcView] = createSignal<NcView>("form");
  const [ncType, setNcType] = createSignal<NcType>("tmux");
  const [ncDir, setNcDir] = createSignal("");
  // Default command is empty ("plain" = inject nothing) — the type toggle
  // decides tmux vs regular; the real commands follow in the dropdown.
  const [ncCmd, setNcCmd] = createSignal<NcCmd>("plain");
  const [ncCustom, setNcCustom] = createSignal("");
  // v0.4.4-beta.2: the Claude-session list is shown ONLY for the dedicated
  // "choose from list" command — NOT for --resume/--continue (those are plain
  // runs). Filter is User / Agent / All (Agent = Task sidechain).
  type NcFilter = "user" | "agent" | "all";
  const [ncSessions, setNcSessions] = createSignal<ClaudeSessionInfo[]>([]);
  const [ncSessionsLoading, setNcSessionsLoading] = createSignal(false);
  const [ncSessionsErr, setNcSessionsErr] = createSignal<string | null>(null);
  const [ncSearch, setNcSearch] = createSignal("");
  const [ncFilter, setNcFilter] = createSignal<NcFilter>("user");
  const [ncPickedSession, setNcPickedSession] = createSignal<ClaudeSessionInfo | null>(null);
  const ncShowsList = (): boolean => ncCmd() === "from-list";
  // v0.4.4-beta.2: SMART [Connect] flow. Clicking Connect first arms the SSH
  // handle headlessly, probes for live tmux sessions, and — if any exist — pops
  // a small picker so the user can RE-ATTACH to one or open a plain terminal.
  // If none exist (or the workspace can't connect headlessly, e.g. password
  // auth) it falls straight through to a regular shell. Reconnect-to-open-tmux
  // lives here, not in the wizard, so the common case is one click.
  const [connectProbing, setConnectProbing] = createSignal(false);
  const [tmuxPick, setTmuxPick] = createSignal<TmuxSessionInfo[] | null>(null);
  const fmtSessionAge = (mt: number): string => {
    if (!mt) return "—";
    const sec = Math.max(1, Math.floor(Date.now() / 1000 - mt));
    if (sec < 60) return `${sec}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m`;
    if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
    return `${Math.floor(sec / 86400)}d`;
  };
  const loadNcSessions = async () => {
    setNcSessionsLoading(true);
    setNcSessionsErr(null);
    try {
      const list = await invoke<ClaudeSessionInfo[]>("pane_list_claude_sessions", {
        workspaceId: p.workspaceId,
        limit: 40,
      });
      setNcSessions(list);
    } catch (e) {
      setNcSessionsErr(String(e));
    } finally {
      setNcSessionsLoading(false);
    }
  };
  const ncFilteredSessions = (): ClaudeSessionInfo[] => {
    const q = ncSearch().trim().toLowerCase();
    const f = ncFilter();
    return ncSessions().filter((s) => {
      if (f === "user" && s.is_subagent) return false;
      if (f === "agent" && !s.is_subagent) return false;
      if (!q) return true;
      return (
        s.session_id.toLowerCase().includes(q) ||
        (s.project_path ?? "").toLowerCase().includes(q) ||
        (s.last_user ?? "").toLowerCase().includes(q) ||
        (s.last_assistant ?? "").toLowerCase().includes(q)
      );
    });
  };
  // When true, the folder picker returns its choice into the new-connection
  // modal (ncDir) instead of connecting immediately.
  const [dirPickForNewConn, setDirPickForNewConn] = createSignal(false);
  // v0.4.4-beta.2: the tmux session picker + smart-connect caret menu were
  // removed — everything now lives in the two-button flow (Connect / Wizard).
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
    // v0.4.4-beta.2: the browser is now an inline VIEW of the new-connection
    // modal — feed the choice into ncDir and switch back to the form view
    // (the modal itself never closed).
    if (dirPickForNewConn()) {
      setDirPickForNewConn(false);
      setNcDir(dir);
      setNcView("form");
      return;
    }
    p.onConnect(p.pane.pane_id, { cwdOverride: dir });
  };
  // v0.4.4-beta.2: cancel the inline browser → back to the form (keep ncDir).
  const cancelBrowse = () => {
    setDirPicker(null);
    setDirPickForNewConn(false);
    setNcView("form");
  };
  // v0.4.4 (Task 2): close the folder picker; if it was serving the
  // new-connection modal, reopen that modal (keeping the previous ncDir).
  const closeDirPicker = () => {
    setDirPicker(null);
    if (dirPickForNewConn()) {
      setDirPickForNewConn(false);
      setNewConnModal(true);
    }
  };
  // v0.4.4-beta.2: open the unified new-connection modal with defaults.
  const openNewConnModal = () => {
    setNcView("form");
    setNcType("tmux");
    setNcDir("");
    setNcCmd("plain");
    setNcCustom("");
    setNcSearch("");
    setNcFilter("user");
    setNcPickedSession(null);
    setNcSessions([]);
    setNcSessionsErr(null);
    setNewConnModal(true);
  };
  // Validation: directory is OPTIONAL (empty = the user's $HOME root, the
  // backend default — fill it only to run elsewhere). custom needs text;
  // "choose from list" needs a session pick. --resume/--continue are plain runs.
  const newConnValid = (): boolean => {
    if (ncCmd() === "custom" && !ncCustom().trim()) return false;
    if (ncCmd() === "from-list" && !ncPickedSession()) return false;
    return true;
  };
  // v0.4.4-beta.2: browse is now an INLINE view within the same modal (not a
  // separate popup). Load the tree into dirPicker() and switch the body.
  const browseNewConnDir = () => {
    setDirPickForNewConn(true);
    setNcView("browse");
    void openDirPicker();
  };
  // v0.4.4-beta.2: auto-load the Claude session list when "choose from list"
  // is picked while the modal is open (once; refresh via the list's ⟳).
  createEffect(() => {
    if (newConnModal() && ncShowsList() && ncSessions().length === 0 && !ncSessionsLoading()) {
      void loadNcSessions();
    }
  });
  // v0.4.4-beta.2: SMART [Connect]. Arm SSH headlessly → probe tmux → branch:
  // live sessions → picker; otherwise a plain regular shell. Local (non-SSH)
  // workspaces have no tmux, so they connect straight away.
  const smartConnect = async () => {
    if (!isSsh()) { p.onConnect(p.pane.pane_id, { persistent: false }); return; }
    setConnectProbing(true);
    try {
      // Idempotent, PTY-free, tmux-free; no-ops on password-auth (can't prompt
      // headlessly) — those simply yield an empty list and connect regular.
      try { await invoke("workspace_ensure_connected", { workspaceId: p.workspaceId }); } catch { /* fall through */ }
      let list: TmuxSessionInfo[] = [];
      try {
        list = await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", { workspaceId: p.workspaceId });
      } catch { list = []; }
      if (list.length > 0) {
        setTmuxPick(list);
      } else {
        p.onConnect(p.pane.pane_id, { persistent: false });
      }
    } finally {
      setConnectProbing(false);
    }
  };
  // Attach to a chosen live tmux session (persistent + name; inject nothing —
  // its shell is already running), or open a plain regular shell.
  const pickTmuxSession = (name: string | null) => {
    setTmuxPick(null);
    if (name) p.onConnect(p.pane.pane_id, { persistent: true, tmuxSession: name });
    else p.onConnect(p.pane.pane_id, { persistent: false });
  };
  // Translate the modal's choices into a single ConnectOpts and connect.
  const submitNewConn = () => {
    if (!newConnValid()) return;
    const opts: ConnectOpts = {};
    // Connection type → persistent flag (tmux=true, regular=false). We do NOT
    // use mode="tmux"/"plain" here: those force the persistence, which would
    // fight the toggle (e.g. a bare-shell command inside a tmux session). The
    // backend's effective_persistent honors the flag when mode isn't tmux/plain.
    opts.persistent = ncType() === "tmux";
    const c = ncCmd();
    const picked = ncPickedSession();
    // A picked resume session overrides the directory with its own project
    // path (so resume lands where the session was created), if absolute.
    let dir = ncDir().trim();
    if (picked && picked.project_path?.startsWith("/")) dir = picked.project_path;
    // Empty stays empty: no cwdOverride → the backend lands in the user's $HOME
    // root (default). Only send an override when the user actually typed a path
    // (or a picked session supplied its project dir).
    if (dir) opts.cwdOverride = dir;
    if (c === "plain") {
      // Bare shell — inject nothing; mode stays undefined so the persistent
      // flag alone decides tmux vs regular.
    } else if (c === "custom") {
      opts.mode = "cmd";
      opts.cmd = ncCustom().trim();
    } else {
      opts.mode = "claude";
      if (picked) opts.claudeArgs = `--resume ${picked.session_id}`;
      else if (c === "claude-continue") opts.claudeArgs = "--continue";
      else if (c === "claude-resume") opts.claudeArgs = "--resume";
      else if (c === "claude-skip") opts.claudeArgs = "--dangerously-skip-permissions";
    }
    setNewConnModal(false);
    p.onConnect(p.pane.pane_id, opts);
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
  const isSshPane = () => isRemoteConn(effectiveConn());

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

  // beta.3 (pane-dragdrop): terminal attach is a createEffect so a
  // pane_id swap (workspace_swap_panes moves the two Pane leaves in
  // the layout tree — same tree slots, different pane_ids in them)
  // detaches the previous terminal container and attaches the new
  // one keyed to the new pane_id. Under the pre-dragdrop code this
  // was in onMount() and would stick to the first pane_id, leaving
  // the wrong xterm mounted after a swap. The xterm instance itself
  // survives in the g_terminals registry across detach/reattach.
  createEffect(() => {
    const paneId = p.pane.pane_id;
    if (!slotRef) return;
    const nextTi = p.ensureTerm(paneId);
    if (ti && ti !== nextTi) {
      // Detach previous terminal's container from THIS slot before
      // hooking up the new one. If it was moved elsewhere already
      // (the other PaneView's effect ran first), parentElement will
      // be that slot — leave it alone.
      if (ti.container.parentElement === slotRef) {
        slotRef.removeChild(ti.container);
      }
    }
    ti = nextTi;
    if (ti.container.parentElement !== slotRef) {
      // If the container is currently hosted in the OTHER slot (mid-
      // swap), detach it there so appendChild here moves it cleanly.
      ti.container.parentElement?.removeChild(ti.container);
      slotRef.appendChild(ti.container);
    }
    ti.container.style.display = "block";
    requestAnimationFrame(() => ti?.fitAndResize());
  });

  onMount(() => {
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
  // beta.3 (pane-dragdrop): reactive drop-zone classes for this pane.
  // A pane can be either the drag SOURCE (.pane-dragging → dim) or
  // the drag TARGET (.pane-drop-target → outline + zone-specific
  // .pane-drop-{center|left|right|top|bottom} for a half-tint hint).
  // MVP: only center performs the swap on release — half-zone visuals
  // hint the future split-creation but currently fall back to swap.
  const paneDragClasses = (): string => {
    const cls: string[] = [];
    if (paneDragStore.dragPaneId() === p.pane.pane_id) cls.push("pane-dragging");
    if (paneDragStore.dropTargetId() === p.pane.pane_id) {
      cls.push("pane-drop-target");
      const z = paneDragStore.dropZone();
      if (z) cls.push(`pane-drop-${z}`);
    }
    return cls.join(" ");
  };

  return (
    <div
      ref={(el) => (paneRef = el)}
      data-pane-id={p.pane.pane_id}
      class={`pane ${p.isActive ? "active" : ""} ${p.isWaiting ? "waiting" : ""} ${p.isNotified ? "pane-pulse" : ""} ${dropping() ? "drop-target" : ""} ${paneDragClasses()}`}
      data-has-color={liveEffective().color ? "true" : "false"}
      style={liveEffective().color ? `--pane-color: ${liveEffective().color}` : undefined}
      onMouseDown={() => {
        // A short click on the header still focuses the pane. A completed
        // drag sets `didDrag` in paneDrag — but focusing during a drag
        // is harmless and, in fact, matches the sidebar's UX (the source
        // stays selected). The workspace_swap_panes command keeps
        // pane_ids stable, so this focus survives the swap unchanged.
        p.onFocus(p.pane.pane_id);
      }}
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
      <div
        class="pane-header"
        onPointerDown={(e) => {
          // beta.3 (pane-dragdrop) Fix 1: the whole header is the drag
          // surface (was just the title span — too small to hit).
          // startPaneDrag is left-button-only and bails on interactive
          // children (buttons / .pane-btn), so their clicks keep working.
          const label =
            p.pane.title
              ?? p.workspaceName
              ?? (p.pane.connection
                ? describeConnection(p.pane.connection)
                : p.workspaceConnection
                  ? describeConnection(p.workspaceConnection)
                  : p.pane.pane_id);
          startPaneDrag(p.pane.pane_id, label, e);
        }}
      >
        {/* Phase 23.I: header fallback chain — user-set pane.title
            beats workspace name beats the raw SSH URL. The old
            describeConnection() output (e.g. "ssh runner@1.2.3.4:22")
            was noisy and only useful for debugging.
            Phase 31: prepend the effective emoji glyph when set.
            beta.3 (pane-dragdrop): this span is also the pane's drag
            handle — pointerdown starts a pointer-drag reorder. A short
            press stays a click (pane focus + no swap); a >5px move
            promotes to a drag and drops on the pane under the cursor.
            Escape / pointercancel abort with no swap. */}
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
            <IconInfo size={14} />
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
          <IconPencil size={14} />
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
              <IconPower size={14} />
            </button>
            <button
              class="pane-btn pane-disc-caret"
              title={t("pane.tooltip.kill_session")}
              onClick={(e) => {
                e.stopPropagation();
                setShowDiscMenu(!showDiscMenu());
              }}
            >
              <IconChevronDown size={13} />
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
          <IconArrowLeftRight size={14} />
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
            <IconMaximize size={13} /> {p.backgroundPaneCount}
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
          {p.isMaximized ? <IconMinimize size={14} /> : <IconMaximize size={14} />}
        </button>
        <button class="pane-btn" title="Split right (Ctrl+Shift+D)" onClick={() => p.onSplit(p.pane.pane_id, "horizontal")}><IconColumns size={14} /></button>
        <button class="pane-btn" title="Split down (Ctrl+Shift+E)" onClick={() => p.onSplit(p.pane.pane_id, "vertical")}><IconRows size={14} /></button>
        {/* Unshipped-fivefer (#4): pop this terminal into its own window.
            Only meaningful for a live session — hidden until connected. */}
        <Show when={p.isConnected}>
          <button
            class="pane-btn"
            title={t("pane.tooltip.popout")}
            onClick={(e) => {
              e.stopPropagation();
              void p.onPopOut(p.pane.pane_id);
            }}
          >
            <IconExternalLink size={14} />
          </button>
        </Show>
        <button class="pane-btn pane-close" title={t("pane.tooltip.close")} onClick={() => p.onClose(p.pane.pane_id)}><IconClose size={14} /></button>
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
                  <h3><IconWarning size={14} /> HOST KEY CHANGED for {hostTrustHere()!.target}</h3>
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
              {/* v0.4.4-beta.2: two buttons only — [Connect] probes for live
                  tmux sessions first (arms SSH headlessly, then lists): if any
                  exist it pops a picker to re-attach or open a plain shell;
                  otherwise it connects a regular shell straight away.
                  [Connection wizard] opens the unified wizard (type / directory
                  / command / resume list). */}
              <div class="connect-buttons">
                <button class="primary big" onClick={() => void smartConnect()} disabled={connectProbing()}>
                  {connectProbing() ? t("connect.probing") : t("common.connect")}
                </button>
                <button class="big nc-wizard-btn" onClick={openNewConnModal} disabled={connectProbing()}>
                  {t("connect.openWizard")}
                </button>
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

      {/* v0.4.4-beta.2: SMART [Connect] tmux picker — appears only when the
          headless probe found live tmux sessions. Re-attach to one, or open a
          plain regular shell. Portal so it stacks above panels/feed. */}
      <Show when={tmuxPick()}>
        <Portal>
          <div class="modal-backdrop nc-backdrop" onClick={() => setTmuxPick(null)}>
            <div class="nc-modal nc-modal-sm" role="dialog" aria-modal="true"
              onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
              <div class="nc-head">
                <h3>{t("connect.tmuxPick.title")}</h3>
                <button class="feed-x" title={t("common.close")} onClick={() => setTmuxPick(null)}><IconClose size={14} /></button>
              </div>
              <div class="nc-body">
                <p class="nc-hint">{t("connect.tmuxPick.hint")}</p>
                <div class="nc-resume-list">
                  <For each={tmuxPick()!}>
                    {(s) => (
                      <div class="nc-resume-row" onClick={() => pickTmuxSession(s.name)} title={s.name}>
                        <div class="nc-resume-head">
                          <span class="nc-resume-proj"><IconTerminal size={14} /> {s.name}</span>
                          <span class="nc-resume-badge">{s.windows}w</span>
                          <Show when={s.attached}>
                            <span class="nc-resume-badge">{t("connect.newConn.tmuxAttached")}</span>
                          </Show>
                          <span class="nc-resume-age">{fmtSessionAge(s.last_attached || s.created)}</span>
                        </div>
                      </div>
                    )}
                  </For>
                </div>
              </div>
              <div class="nc-footer">
                <button onClick={() => pickTmuxSession(null)}><IconTerminal size={14} /> {t("connect.tmuxPick.regular")}</button>
                <button onClick={() => setTmuxPick(null)}>{t("common.cancel")}</button>
              </div>
            </div>
          </div>
        </Portal>
      </Show>

      {/* v0.4.4-beta.2 (Task 2 polish): unified new-connection wizard —
          connection type + directory + command in one modal. Rendered through
          a Portal onto <body> so its own high z-index stacks above the
          sidebar / panels / feed regardless of the pane's local stacking. */}
      <Show when={newConnModal()}>
        <Portal>
          <div
            class="nc-backdrop"
            onClick={() => setNewConnModal(false)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setNewConnModal(false);
            }}
          >
            <div
              class="nc-modal"
              role="dialog"
              aria-modal="true"
              aria-label={t("connect.newConn.title")}
              onClick={(e) => e.stopPropagation()}
              onMouseDown={(e) => e.stopPropagation()}
              onKeyDown={(e) => {
                if (e.key === "Escape") { e.stopPropagation(); setNewConnModal(false); }
                // Enter submits, except while typing in the custom-command field.
                if (
                  e.key === "Enter" &&
                  (e.target as HTMLElement)?.tagName !== "SELECT"
                ) {
                  e.preventDefault();
                  submitNewConn();
                }
              }}
            >
              <div class="nc-head">
                <h3>{t("connect.newConn.title")}</h3>
                <button class="feed-x" title={t("common.close")} onClick={() => setNewConnModal(false)}><IconClose size={14} /></button>
              </div>

              <div class="nc-body">
                {/* ── FORM view ─────────────────────────────────────── */}
                <Show when={ncView() === "form"}>
                  {/* 1. Connection type (Regular | TMUX) */}
                  <div class="nc-section">
                    <label class="nc-label">{t("connect.newConn.type")}</label>
                    <div class="nc-segmented" role="tablist">
                      <button
                        role="tab"
                        aria-selected={ncType() === "regular"}
                        class={`nc-seg ${ncType() === "regular" ? "active" : ""}`}
                        onClick={() => setNcType("regular")}
                      >
                        <IconTerminal size={14} /> {t("connect.newConn.typeRegular")}
                      </button>
                      <button
                        role="tab"
                        aria-selected={ncType() === "tmux"}
                        class={`nc-seg ${ncType() === "tmux" ? "active" : ""}`}
                        onClick={() => setNcType("tmux")}
                      >
                        <IconTerminal size={14} /> {t("connect.newConn.typeTmux")}
                      </button>
                    </div>
                    <p class="nc-hint">
                      {ncType() === "tmux"
                        ? t("connect.newConn.typeTmux.hint")
                        : t("connect.newConn.typeRegular.hint")}
                    </p>
                  </div>

                  {/* 2. Directory (optional — empty = the user's $HOME root) */}
                  <div class="nc-section">
                    <label class="nc-label">
                      {t("connect.newConn.directory")}{" "}
                      <span class="nc-optional">{t("connect.newConn.dirDefault")}</span>
                    </label>
                    <div class="nc-dir-row">
                      <input
                        class="nc-input"
                        autofocus
                        placeholder="/home/user/project"
                        value={ncDir()}
                        onInput={(e) => setNcDir(e.currentTarget.value)}
                      />
                      <Show when={isSsh()}>
                        <button class="nc-browse" onClick={browseNewConnDir}>
                          {t("connect.newConn.browse")}
                        </button>
                      </Show>
                    </div>
                  </div>

                  {/* 3. Command (dropdown; custom field only when chosen) */}
                  <div class="nc-section">
                    <label class="nc-label">{t("connect.newConn.command")}</label>
                    <select
                      class="nc-select"
                      value={ncCmd()}
                      onChange={(e) => { setNcCmd(e.currentTarget.value as NcCmd); setNcPickedSession(null); }}
                    >
                      <option value="plain"></option>
                      <option value="claude">claude</option>
                      <option value="claude-continue">claude --continue</option>
                      <option value="claude-resume">claude --resume</option>
                      <option value="claude-skip">claude --dangerously-skip-permissions</option>
                      <option value="from-list">{t("connect.newConn.fromList")}</option>
                      <option value="custom">{t("connect.newConn.custom")}</option>
                    </select>
                    <Show when={ncCmd() === "custom"}>
                      <input
                        class="nc-input nc-custom"
                        placeholder="npm run dev"
                        value={ncCustom()}
                        onInput={(e) => setNcCustom(e.currentTarget.value)}
                      />
                    </Show>
                  </div>

                  {/* 4. Session list — only for the "choose from list" command */}
                  <Show when={ncShowsList()}>
                    <div class="nc-section">
                      <label class="nc-label">
                        {t("connect.newConn.resumeTitle")} <span class="nc-req">*</span>
                      </label>
                      <div class="nc-resume-tools">
                        <input
                          class="nc-input nc-search"
                          placeholder={t("connect.newConn.search")}
                          value={ncSearch()}
                          onInput={(e) => setNcSearch(e.currentTarget.value)}
                        />
                        <div class="nc-segmented nc-filter">
                          <For each={[
                            { v: "user", label: t("connect.newConn.filterUser") },
                            { v: "agent", label: t("connect.newConn.filterAgent") },
                            { v: "all", label: t("connect.newConn.filterAll") },
                          ] as { v: NcFilter; label: string }[]}>
                            {(f) => (
                              <button
                                class={`nc-seg ${ncFilter() === f.v ? "active" : ""}`}
                                onClick={() => setNcFilter(f.v)}
                              >
                                {f.label}
                              </button>
                            )}
                          </For>
                        </div>
                        <button class="nc-browse" title={t("connect.newConn.refresh")} onClick={() => void loadNcSessions()}><IconRefresh size={14} /></button>
                      </div>
                      <div class="nc-resume-list">
                        <Show when={ncSessionsLoading()}>
                          <p class="nc-muted">{t("claude_picker.loading")}</p>
                        </Show>
                        <Show when={ncSessionsErr()}>
                          <p class="nc-muted err"><IconWarning size={13} /> {ncSessionsErr()}</p>
                        </Show>
                        <Show when={!ncSessionsLoading() && !ncSessionsErr() && ncFilteredSessions().length === 0}>
                          <p class="nc-muted">{t("claude_picker.empty")}</p>
                        </Show>
                        <For each={ncFilteredSessions()}>
                          {(s) => (
                            <div
                              class={`nc-resume-row ${ncPickedSession()?.session_id === s.session_id ? "picked" : ""}`}
                              onClick={() => setNcPickedSession(s)}
                              title={s.jsonl_path}
                            >
                              <div class="nc-resume-head">
                                <code class="nc-resume-id">{s.session_id.slice(0, 8)}</code>
                                <Show when={s.is_subagent}>
                                  <span class="nc-resume-badge">{t("connect.newConn.filterAgent")}</span>
                                </Show>
                                <span class="nc-resume-proj">{s.project_path}</span>
                                <span class="nc-resume-age">{fmtSessionAge(s.mtime_unix)}</span>
                              </div>
                              <Show when={s.last_user}>
                                <div class="nc-resume-prev">{s.last_user}</div>
                              </Show>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                </Show>

                {/* ── BROWSE view (inline folder tree) ──────────────── */}
                <Show when={ncView() === "browse" && dirPicker()}>
                  <div class="nc-section">
                    <div class="nc-browse-path" title={dirPicker()!.path}>{dirPicker()!.path}</div>
                    <Show when={recentDirs().length > 0}>
                      <div class="nc-recent">
                        <For each={recentDirs()}>
                          {(d) => (
                            <button class="nc-recent-row" title={d} onClick={() => chooseDir(d)}><IconClock size={14} /> {d}</button>
                          )}
                        </For>
                      </div>
                    </Show>
                    <Show when={dirPicker()!.error}>
                      <p class="nc-muted err"><IconWarning size={13} /> {dirPicker()!.error}</p>
                    </Show>
                    <ul class="nc-dir-list">
                      <Show when={dirPicker()!.path !== "/"}>
                        <li class="nc-dir-item up" onClick={() => void navigateDirPicker(dirPickerParent(dirPicker()!.path))}><IconFolder size={14} /> ..</li>
                      </Show>
                      <For each={dirPicker()!.dirs}>
                        {(name) => (
                          <li class="nc-dir-item" onClick={() => void navigateDirPicker(dirPickerJoin(dirPicker()!.path, name))}><IconFolder size={14} /> {name}</li>
                        )}
                      </For>
                      <Show when={!dirPicker()!.loading && dirPicker()!.dirs.length === 0 && !dirPicker()!.error}>
                        <li class="nc-dir-empty">{t("connect.dirPicker.empty")}</li>
                      </Show>
                    </ul>
                  </div>
                </Show>
              </div>

              <div class="nc-footer">
                <Show
                  when={ncView() === "browse"}
                  fallback={
                    <>
                      <button class="nc-btn" onClick={() => setNewConnModal(false)}>{t("common.cancel")}</button>
                      <button class="nc-btn primary" disabled={!newConnValid()} onClick={submitNewConn}>
                        {t("common.connect")}
                      </button>
                    </>
                  }
                >
                  <button class="nc-btn" onClick={cancelBrowse}>{t("connect.newConn.back")}</button>
                  <button class="nc-btn primary" disabled={!dirPicker()} onClick={() => dirPicker() && chooseDir(dirPicker()!.path)}>
                    {t("connect.dirPicker.useThis")}
                  </button>
                </Show>
              </div>
            </div>
          </div>
        </Portal>
      </Show>

      {/* Phase 65 (bug AA): remote folder picker for "Open in directory". */}
      {/* v0.4.4-beta.2: only the standalone "open dir" flow uses this popup;
          the new-connection wizard renders the tree inline (ncView="browse"). */}
      <Show when={dirPicker() && !dirPickForNewConn()}>
        <div class="modal-backdrop" onClick={closeDirPicker}>
          <div
            class="modal claude-picker"
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <div class="settings-head">
              <h3>{t("connect.dirPicker.title")}</h3>
              <button class="feed-x" title={t("common.close")} onClick={closeDirPicker}><IconClose size={14} /></button>
            </div>
            <div class="dir-picker-path" title={dirPicker()!.path}>{dirPicker()!.path}</div>
            <Show when={recentDirs().length > 0}>
              <div class="dir-picker-recent">
                <div class="dir-picker-recent-label">{t("connect.dirPicker.recent")}</div>
                <For each={recentDirs()}>
                  {(d) => (
                    <button class="dir-picker-recent-row" title={d} onClick={() => chooseDir(d)}>
                      <IconClock size={14} /> {d}
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
                <p class="status-line err"><IconWarning size={13} /> {dirPicker()!.error}</p>
              </Show>
              <ul class="dir-picker-list">
                <Show when={dirPicker()!.path !== "/"}>
                  <li class="dir-picker-row up" onClick={() => void navigateDirPicker(dirPickerParent(dirPicker()!.path))}>
                    <IconFolder size={14} /> ..
                  </li>
                </Show>
                <For each={dirPicker()!.dirs}>
                  {(name) => (
                    <li
                      class="dir-picker-row"
                      onClick={() => void navigateDirPicker(dirPickerJoin(dirPicker()!.path, name))}
                    >
                      <IconFolder size={14} /> {name}
                    </li>
                  )}
                </For>
                <Show when={!dirPicker()!.loading && dirPicker()!.dirs.length === 0 && !dirPicker()!.error}>
                  <li class="dir-picker-empty">{t("connect.dirPicker.empty")}</li>
                </Show>
              </ul>
            </div>
            <div class="modal-buttons">
              <button onClick={closeDirPicker}>{t("common.cancel")}</button>
              <button class="primary" onClick={() => chooseDir(dirPicker()!.path)}>
                {t("connect.dirPicker.useThis")}
              </button>
            </div>
          </div>
        </div>
      </Show>

      {/* v0.4.4-beta.2: standalone Claude session picker + tmux session picker
          removed — session resume lives in the wizard ("choose from list"),
          and connections go through the two-button Connect / Wizard flow. */}
    </div>
  );
}
