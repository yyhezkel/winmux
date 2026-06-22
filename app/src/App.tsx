import { createEffect, createSignal, ErrorBoundary, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Sidebar } from "./Sidebar";
import { CreateWorkspaceModal } from "./CreateWorkspaceModal";
import { LayoutView } from "./LayoutView";
import { FeedPanel } from "./FeedPanel";
import { NotesModal } from "./NotesModal";
import { ProvisioningWizard } from "./ProvisioningWizard";
import { SettingsModal } from "./SettingsModal";
import { SshKeyOfferModal } from "./SshKeyOfferModal";
import { CommandPalette, type Command } from "./CommandPalette";
import { PortsWindow } from "./PortsWindow";
import { BrowserWindow } from "./BrowserWindow";
import { FileManagerWindow } from "./FileManagerWindow";
import { ConnectExistingWizard } from "./ConnectExistingWizard";
import {
  TerminalInstance,
  copyTerminalSelection,
  pasteIntoActiveTerminal,
  setCtrlCCopyOnSelect,
} from "./terminalInstance";
import {
  applyTheme,
  loadSettings,
  saveSettings,
  DEFAULT_SHORTCUTS,
  DEFAULT_HOOKS_UPDATES,
  type Settings,
  type SidebarMode,
  type UpdateInfo,
  type HooksOutdatedInfo,
} from "./settings";
import { applyI18nSettings, t } from "./i18n";
import { buildShortcutTable, keyEq, matches, parseShortcut, type ParsedShortcut } from "./shortcuts";
import { makeSttRecorder, type SttRecorder } from "./stt";
import {
  collectPanes,
  describeConnection,
  effectiveIdentity,
  findPane,
  type Connection,
  type EnvVar,
  type FeedItem,
  type ForwardRow,
  type FeedResolvedEvent,
  type LayoutNode,
  type Note,
  type NotesFile,
  type PtyDataEvent,
  type PtyExitEvent,
  type SplitDirection,
  type Workspace,
  type WorkspacesFile,
} from "./types";
import "@xterm/xterm/css/xterm.css";
import "./App.css";

type PaneStatus = { msg: string; err: boolean };

// Phase 62.B (item I): sidebar is a 3-state control — full / icons /
// hidden. The MODE persists in settings.json (atomic, Rule #7 — see
// settings.rs `sidebar_mode`). The full-mode WIDTH (continuous drag
// geometry) stays in localStorage, the right home for per-machine
// pixel geometry.
const SIDEBAR_MIN_W = 160;
const SIDEBAR_MAX_W = 480;
const SIDEBAR_DEFAULT_W = 224;
const SIDEBAR_ICONS_W = 48;
const SIDEBAR_W_KEY = "winmux.sidebar-width";
function loadSidebarWidth(): number {
  try {
    const n = Number(localStorage.getItem(SIDEBAR_W_KEY));
    if (Number.isFinite(n) && n >= SIDEBAR_MIN_W && n <= SIDEBAR_MAX_W) return n;
  } catch {
    // ignore
  }
  return SIDEBAR_DEFAULT_W;
}

function App() {
  const [file, setFile] = createSignal<WorkspacesFile>({
    version: 1,
    active_workspace_id: null,
    workspaces: [],
  });
  const [showCreate, setShowCreate] = createSignal(false);
  const [editingWorkspace, setEditingWorkspace] = createSignal<Workspace | null>(null);
  const [activePaneId, setActivePaneId] = createSignal<string | null>(null);
  // Phase 55-A: pane maximize toggle. When set, LayoutView gets just
  // that leaf as its node (the rest of the split tree still lives in
  // ws.layout; restore swaps it back). pty_resize fires for every
  // pane in the workspace after enter/exit so xterm geometry catches
  // up to the new available area.
  const [maximizedPaneId, setMaximizedPaneId] = createSignal<string | null>(null);
  const [pendingPwFor, setPendingPwFor] = createSignal<string | null>(null);
  const [pendingPassphraseFor, setPendingPassphraseFor] = createSignal<{
    paneId: string;
    keyPath: string;
    bad?: boolean;
  } | null>(null);
  const [pendingHostTrust, setPendingHostTrust] = createSignal<{
    paneId: string;
    target: string;
    keyType: string;
    fingerprint: string;
    mismatchOld?: string;
  } | null>(null);
  const [paneStatus, setPaneStatus] = createSignal<Record<string, PaneStatus>>({});
  // Live pane status text (e.g. "bootstrapping winmux…") set by backend events.
  const [paneStatusText, setPaneStatusText] = createSignal<Record<string, string>>({});
  // Phase 6.5: agent feed (most recent first; capped to 50 server-side).
  const [feedItems, setFeedItems] = createSignal<FeedItem[]>([]);
  // Phase 7.B: notes
  const [notes, setNotes] = createSignal<Note[]>([]);
  const [showNotes, setShowNotes] = createSignal(false);
  // Phase 9.A: settings + Phase 9.B: update banner.
  const [settings, setSettings] = createSignal<Settings | null>(null);
  const [showSettings, setShowSettings] = createSignal(false);
  const [updateBanner, setUpdateBanner] = createSignal<UpdateInfo | null>(null);
  // Phase 27: in-flight state for the one-click installer download.
  const [installingUpdate, setInstallingUpdate] = createSignal(false);
  const installUpdate = async () => {
    if (installingUpdate()) return;
    setInstallingUpdate(true);
    try {
      // Backend will exit() the app ~800ms after this returns; the
      // invoke promise resolves before exit so we can show "downloading"
      // → "installing" cleanly. On error the app keeps running.
      await invoke("download_and_install_update");
      // We're still alive briefly; the user sees the button locked in
      // "downloading…" state until the process actually exits.
    } catch (e) {
      flashSummaryToast("err", t("update_banner.install_failed", { msg: String(e) }));
      setInstallingUpdate(false);
    }
  };
  // Phase 14.A: server provisioning wizard.
  const [showProvision, setShowProvision] = createSignal(false);
  // Phase 65.C: "Connect to existing server" wizard. null = closed; the
  // object carries optional add-to-existing context (workspace id +
  // prefill of the known host/user/port).
  const [connectExisting, setConnectExisting] = createSignal<{
    existingWorkspaceId?: string;
    prefill?: { host?: string; port?: number; user?: string };
  } | null>(null);
  // Phase 35 (#1.3): command palette (Ctrl+Shift+P).
  const [showPalette, setShowPalette] = createSignal(false);
  // Phase 36 (#2.2): live auto port-forwards (all workspaces).
  const [portForwards, setPortForwards] = createSignal<ForwardRow[]>([]);
  // Phase 46: ports the remote watcher has reported but the user
  // hasn't chosen to forward yet (one click → forward + browser).
  const [detectedPorts, setDetectedPorts] = createSignal<
    { workspace_id: string; remote_port: number; addr: string; family: string }[]
  >([]);
  // Phase 40: floating Ports window — scoped to the active workspace.
  const [showPortsWindow, setShowPortsWindow] = createSignal(false);
  // Phase 53 (rebased): floating workspace-level Browser window. Each
  // workspace owns its own browser session + remembered geometry; the
  // signal tracks the open/closed visibility of the host shell only
  // (the native Webview is hidden on close, not destroyed — page state
  // survives across open/close cycles).
  const [showBrowserWindow, setShowBrowserWindow] = createSignal(false);
  // Phase 53 (rebased): workspace-level File Manager floating window.
  // Pure HTML — wraps the existing FileManagerPane component.
  const [showFilesWindow, setShowFilesWindow] = createSignal(false);
  // Phase 62.B (item I): sidebar mode lives in settings.json; full-mode
  // width lives in localStorage. Phase 65.P: two modes only (full /
  // icons). Any legacy "hidden" value migrates to "icons" on read so
  // older settings.json files don't strand the sidebar off-screen.
  const [sidebarWidth, setSidebarWidth] = createSignal(loadSidebarWidth());
  const sidebarMode = (): SidebarMode => {
    // Read as a plain string: a legacy settings.json may still hold the
    // dropped "hidden" value, which is outside the SidebarMode union.
    const raw = settings()?.sidebar_mode as string | undefined;
    return raw === "icons" || raw === "hidden" ? "icons" : "full";
  };
  const sidebarPx = () => {
    const m = sidebarMode();
    if (m === "icons") return SIDEBAR_ICONS_W;
    return sidebarWidth();
  };
  const setSidebarMode = (mode: SidebarMode) => {
    const s = settings();
    if (!s) return;
    const next: Settings = { ...s, sidebar_mode: mode };
    setSettings(next);
    void saveSettings(next).catch((e) =>
      console.warn("saveSettings (sidebar_mode) failed", e),
    );
  };
  // Phase 65.P: Ctrl+B toggles full ↔ icons (two modes only); the
  // header button does the same. No "hidden" state anymore.
  const cycleSidebarMode = () => {
    setSidebarMode(sidebarMode() === "full" ? "icons" : "full");
  };
  createEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_W_KEY, String(sidebarWidth()));
    } catch {
      // ignore (quota / private mode)
    }
  });
  const startSidebarResize = (e: MouseEvent) => {
    e.preventDefault();
    // Direction-aware: in RTL the sidebar sits on the right, so its
    // width grows as the pointer moves LEFT — measure from the correct
    // edge.
    const rtl =
      getComputedStyle(document.documentElement).direction === "rtl";
    const onMove = (ev: MouseEvent) => {
      const raw = rtl ? window.innerWidth - ev.clientX : ev.clientX;
      setSidebarWidth(
        Math.max(SIDEBAR_MIN_W, Math.min(SIDEBAR_MAX_W, Math.round(raw))),
      );
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  // Phase 58: push-to-talk voice input. Active recorder instance +
  // listening indicator. The recorder is created lazily on keydown
  // and reused for the lifetime of the press; release fires stop()
  // which resolves the start() promise with the transcribed text.
  let sttRecorder: SttRecorder | null = null;
  const [sttListening, setSttListening] = createSignal(false);
  const [sttError, setSttError] = createSignal<string | null>(null);
  const stopForward = (workspaceId: string, remotePort: number) => {
    void invoke("port_forward_stop", { workspaceId, remotePort });
  };
  // Phase 46: open a forward on demand from PortsWindow. The backend
  // sanity-probes the local port before returning, so on success we
  // know the browser tab will actually reach something. Returns the
  // assigned local port (or throws).
  const startForward = (workspaceId: string, remotePort: number): Promise<number> =>
    invoke<number>("forward_port_start", { workspaceId, remotePort });
  // Phase 35: webview zoom factor for view.zoom.* palette commands.
  const [zoomFactor, setZoomFactor] = createSignal(1);
  const applyZoom = (f: number) => {
    const clamped = Math.max(0.3, Math.min(3, f));
    setZoomFactor(clamped);
    void getCurrentWebview().setZoom(clamped).catch((e) => console.warn("setZoom failed", e));
  };
  // Phase 18: hooks-outdated banners — at most one banner per agent
  // at a time; the user dismisses (skip-this-version persists), defers
  // (banner gone until next connect), or triggers an in-place update.
  const [hooksBanner, setHooksBanner] = createSignal<HooksOutdatedInfo | null>(null);
  const [hooksUpdating, setHooksUpdating] = createSignal(false);
  // Phase 53 (rebased): native child Webviews always paint above
  // HTML, so opening a modal would visually hide it behind the
  // workspace-level Browser window. This derived signal collects
  // every "is a modal open" state; the effect below hides every
  // workspace's Browser Webview when any modal opens. Re-show on
  // close is owned by the BrowserWindow component (Phase 53.E) — its
  // own visibility effect re-calls `workspace_browser_show` with the
  // current rect once `anyModalOpen()` flips back to false.
  const anyModalOpen = () =>
    showCreate() || showNotes() || showSettings() || showProvision() ||
    showPalette() || showPortsWindow() || installingUpdate();
  createEffect(() => {
    if (!anyModalOpen()) return;
    // Broadcast hide to every workspace's Browser Webview. At most
    // one is actually visible at a time (the active workspace's), but
    // hiding any others that may exist is a cheap no-op on the
    // backend side (the command silently ignores workspaces with no
    // Webview spawned).
    for (const w of file().workspaces) {
      void invoke("workspace_browser_hide", {
        workspaceId: w.id,
      }).catch(() => {});
    }
  });

  // Phase 17: ephemeral toast for "Summary saved as note" + the
  // ad-hoc errors that can come back from `claude_summarize`. Auto-
  // dismisses after 4s.
  const [summaryToast, setSummaryToast] = createSignal<
    | { kind: "ok"; text: string }
    | { kind: "err"; text: string }
    | null
  >(null);
  let summaryToastTimer: number | null = null;
  const flashSummaryToast = (kind: "ok" | "err", text: string) => {
    if (summaryToastTimer) clearTimeout(summaryToastTimer);
    setSummaryToast({ kind, text });
    summaryToastTimer = window.setTimeout(() => setSummaryToast(null), 4500);
  };
  // Phase 18: hooks-outdated banner actions.
  const triggerHooksUpdate = async () => {
    const b = hooksBanner();
    if (!b) return;
    setHooksUpdating(true);
    try {
      // Pipe the setup-hooks command through the active SSH pane via
      // the existing tunnel by reusing the connect-with-cmd path. We
      // can't shell out from Rust without an SSH handle; the user's
      // own pane runs the CLI under their PATH (which AddWinmuxToPath
      // sets up). The command writes settings.json, then a fresh
      // restart of Claude picks up the new hooks.
      await invoke("ssh_exec_in_workspace", {
        workspaceId: b.workspace_id,
        cmd: "winmux setup-hooks --agent claude --force --source github",
      }).catch(async () => {
        // Older builds without ssh_exec_in_workspace — fall back to a
        // pane.send: ask the user to run the command themselves.
        console.warn("ssh_exec_in_workspace not available; user must run manually");
      });
      flashSummaryToast("ok", t("hooks_update.toast_done", { version: b.latest }));
      setHooksBanner(null);
    } catch (e) {
      flashSummaryToast("err", String(e));
    } finally {
      setHooksUpdating(false);
    }
  };

  const dismissHooksLater = () => setHooksBanner(null);

  const skipHooksVersion = async () => {
    const b = hooksBanner();
    if (!b) return;
    const s = settings();
    if (!s) {
      setHooksBanner(null);
      return;
    }
    const next: Settings = {
      ...s,
      hooks_updates: {
        ...(s.hooks_updates ?? DEFAULT_HOOKS_UPDATES),
        dismissed: {
          ...(s.hooks_updates?.dismissed ?? {}),
          [b.agent]: Array.from(
            new Set([
              ...((s.hooks_updates?.dismissed ?? {})[b.agent] ?? []),
              b.latest,
            ])
          ),
        },
      },
    };
    try {
      await saveSettings(next);
    } catch (e) {
      console.warn("saveSettings failed (skipHooksVersion)", e);
    }
    setHooksBanner(null);
  };

  const summarizeActivePane = async () => {
    const ws = activeWs();
    if (!ws) {
      flashSummaryToast("err", t("claude.summary.no_workspace"));
      return;
    }
    try {
      const r: any = await invoke("claude_summarize", {
        workspaceId: ws.id,
        paneId: activePaneId() ?? null,
        sessionId: null,
        historyCount: null,
        promptOverride: null,
      });
      flashSummaryToast(
        "ok",
        t("claude.summary.toast", { count: r.messages_count ?? "" }),
      );
      // Refresh notes so the new summary note is visible in the
      // Notes modal next time it opens.
      void refreshNotes();
    } catch (e) {
      flashSummaryToast("err", String(e));
    }
  };
  // Phase 16: parsed shortcut accelerators, rebuilt on every settings
  // load + settings:changed event. Backfilled with DEFAULT_SHORTCUTS
  // when the field is missing (pre-16 settings.json).
  const [shortcutTable, setShortcutTable] = createSignal<
    Record<string, ParsedShortcut | null>
  >(buildShortcutTable(DEFAULT_SHORTCUTS));
  // Phase 11.A: per-pane tmux persistence map { pane_id → session_name }.
  const [panePersistence, setPanePersistence] = createSignal<Record<string, string>>({});
  const refreshPersistence = async () => {
    try {
      const m = await invoke<Record<string, string>>("pane_persistence_list");
      setPanePersistence(m ?? {});
    } catch (e) {
      console.warn("pane_persistence_list failed", e);
    }
  };
  const refreshNotes = async () => {
    try {
      const f = await invoke<NotesFile>("notes_load");
      setNotes(f.notes ?? []);
    } catch (e) {
      console.warn("notes_load failed", e);
    }
  };
  const FEED_AUTO_DISMISS_MS = 3000;
  const scheduleFeedDismiss = (request_id: string) => {
    setTimeout(() => {
      setFeedItems((prev) => prev.filter((i) => i.request_id !== request_id));
    }, FEED_AUTO_DISMISS_MS);
  };
  const [tick, setTick] = createSignal(0);
  const bump = () => setTick(tick() + 1);

  const terms = new Map<string, TerminalInstance>();
  const paneToSession = new Map<string, string>();
  const sessionToPane = new Map<string, string>();

  const ensureTerm = (paneId: string): TerminalInstance => {
    let ti = terms.get(paneId);
    if (!ti) {
      ti = new TerminalInstance(paneId);
      terms.set(paneId, ti);
    }
    return ti;
  };

  const setStatus = (paneId: string, msg: string, err: boolean) =>
    setPaneStatus({ ...paneStatus(), [paneId]: { msg, err } });
  const clearStatus = (paneId: string) => {
    const s = { ...paneStatus() };
    delete s[paneId];
    setPaneStatus(s);
  };

  const activeWs = (): Workspace | null =>
    file().workspaces.find((w) => w.id === file().active_workspace_id) ?? null;

  // Phase 35 (#1.3): cycle focus through the active workspace's panes.
  const focusAdjacentPane = (delta: number) => {
    const ws = activeWs();
    if (!ws?.layout) return;
    const panes = collectPanes(ws.layout);
    if (panes.length === 0) return;
    const cur = activePaneId();
    const idx = cur ? panes.indexOf(cur) : -1;
    const next = panes[(idx + delta + panes.length) % panes.length];
    if (next) setActivePaneId(next);
  };

  // Phase 48-E: find the pane that's the nearest neighbor of `paneId`
  // in a given direction. Walks the layout tree: collects the path
  // from root to the pane (leaf-first), then finds the closest
  // ancestor whose split direction matches and where our subtree sits
  // on the side opposite the target direction. Returns the
  // first/leftmost/topmost leaf of the sibling subtree on the target
  // side. Returns null if no neighbor exists in that direction.
  const findDirectionalNeighbor = (
    root: LayoutNode,
    paneId: string,
    dir: "left" | "right" | "up" | "down",
  ): string | null => {
    const path: { node: LayoutNode & { kind: "split" }; side: "first" | "second" }[] = [];
    const walk = (n: LayoutNode): boolean => {
      if (n.kind === "pane") return n.pane_id === paneId;
      if (walk(n.first)) {
        path.push({ node: n, side: "first" });
        return true;
      }
      if (walk(n.second)) {
        path.push({ node: n, side: "second" });
        return true;
      }
      return false;
    };
    if (!walk(root)) return null;
    const needSplitDir = dir === "left" || dir === "right" ? "horizontal" : "vertical";
    // To go RIGHT/DOWN we need to be on the FIRST side of a matching
    // split; the sibling on the SECOND side holds our neighbor. Reverse
    // for LEFT/UP. Then descend into the sibling: for LEFT/UP, take
    // SECOND repeatedly (rightmost/bottommost leaf); for RIGHT/DOWN,
    // take FIRST repeatedly (leftmost/topmost leaf).
    const seekSide = dir === "right" || dir === "down" ? "first" : "second";
    const descendSide = dir === "right" || dir === "down" ? "first" : "second";
    for (const step of path) {
      if (step.node.direction === needSplitDir && step.side === seekSide) {
        let cur: LayoutNode = step.side === "first" ? step.node.second : step.node.first;
        while (cur.kind === "split") {
          cur = (cur as Extract<LayoutNode, { kind: "split" }>)[descendSide];
        }
        return cur.pane_id;
      }
    }
    return null;
  };

  // Phase 48-E: Ctrl+Alt+Arrow — if there's a pane in that direction,
  // focus it; otherwise split the current pane in that direction.
  // Left/Right map to horizontal splits, Up/Down to vertical.
  const splitOrMove = (dir: "left" | "right" | "up" | "down") => {
    const ws = activeWs();
    const cur = activePaneId();
    if (!ws?.layout || !cur) return;
    const neighbor = findDirectionalNeighbor(ws.layout, cur, dir);
    if (neighbor) {
      setActivePaneId(neighbor);
      return;
    }
    const splitDir: SplitDirection =
      dir === "left" || dir === "right" ? "horizontal" : "vertical";
    void splitPane(cur, splitDir);
  };

  // Phase 35 (#1.3): the command-palette catalog. Each command reuses
  // the same handler the existing UI calls. `enabled` hides commands
  // that need context they don't have (no active workspace / pane).
  const paletteCommands = (): Command[] => {
    const ws = activeWs();
    const pid = activePaneId();
    const hasWs = !!ws;
    const hasPane = !!pid;
    return [
      { id: "workspace.new", label: t("cmd.workspace.new"), handler: () => setShowCreate(true) },
      { id: "workspace.rename", label: t("cmd.workspace.rename"), enabled: () => hasWs, handler: () => { if (ws) { setEditingWorkspace(ws); setShowCreate(true); } } },
      { id: "workspace.disconnect", label: t("cmd.workspace.disconnect"), enabled: () => hasWs, handler: () => { if (ws) void handleDisconnectWorkspace(ws.id); } },
      { id: "workspace.delete", label: t("cmd.workspace.delete"), enabled: () => hasWs, handler: () => { if (ws) void handleDelete(ws.id); } },
      { id: "pane.split.right", label: t("cmd.pane.split.right"), enabled: () => hasPane, handler: () => { if (pid) void splitPane(pid, "horizontal"); } },
      { id: "pane.split.down", label: t("cmd.pane.split.down"), enabled: () => hasPane, handler: () => { if (pid) void splitPane(pid, "vertical"); } },
      { id: "pane.close", label: t("cmd.pane.close"), enabled: () => hasPane, handler: () => { if (pid) void closePane(pid); } },
      { id: "pane.focus.next", label: t("cmd.pane.focus.next"), enabled: () => hasPane, handler: () => focusAdjacentPane(1) },
      { id: "pane.focus.prev", label: t("cmd.pane.focus.prev"), enabled: () => hasPane, handler: () => focusAdjacentPane(-1) },
      // Phase 55-A: maximize toggle (Ctrl+Enter / double-click pane content).
      { id: "pane.maximize", label: t("cmd.pane.maximize"), enabled: () => hasPane, handler: () => toggleMaximize() },
      // Phase 55-B: distribute splits evenly (Ctrl+Alt+=).
      { id: "pane.distributeEvenly", label: t("cmd.pane.distributeEvenly"), enabled: () => hasPane, handler: () => void distributeEvenly() },
      { id: "pane.rename", label: t("cmd.pane.rename"), enabled: () => hasPane, handler: () => { if (pid) window.dispatchEvent(new CustomEvent("winmux:pane-rename", { detail: pid })); } },
      { id: "ssh.connect", label: t("cmd.ssh.connect"), enabled: () => hasPane, handler: () => { if (pid) void connectPane(pid); } },
      { id: "ssh.disconnect", label: t("cmd.ssh.disconnect"), enabled: () => hasPane, handler: () => { if (pid) void disconnectPane(pid); } },
      { id: "ssh.provision", label: t("cmd.ssh.provision"), handler: () => setShowProvision(true) },
      { id: "settings.open", label: t("cmd.settings.open"), handler: () => setShowSettings(true) },
      { id: "settings.language", label: t("cmd.settings.language"), handler: () => setShowSettings(true) },
      { id: "settings.theme", label: t("cmd.settings.theme"), handler: () => setShowSettings(true) },
      { id: "view.zoom.in", label: t("cmd.view.zoom.in"), handler: () => applyZoom(zoomFactor() + 0.1) },
      { id: "view.zoom.out", label: t("cmd.view.zoom.out"), handler: () => applyZoom(zoomFactor() - 0.1) },
      { id: "view.zoom.reset", label: t("cmd.view.zoom.reset"), handler: () => applyZoom(1) },
      { id: "fm.open", label: t("cmd.fm.open"), enabled: () => hasPane && hasWs, handler: () => {
        if (ws && pid) void invoke("workspace_split", { workspaceId: ws.id, paneId: pid, direction: "horizontal", paneKind: "filemanager", browserUrl: null, helpTopic: null });
      } },
    ];
  };

  const connectedPanes = (): Set<string> => {
    void tick();
    return new Set(paneToSession.keys());
  };

  const liveWorkspaceIds = (): Set<string> => {
    void tick();
    const live = new Set<string>();
    for (const w of file().workspaces) {
      if (!w.layout) continue;
      const ps = collectPanes(w.layout);
      if (ps.some((p) => paneToSession.has(p))) live.add(w.id);
    }
    return live;
  };

  // Phase 26: pane_ids with a pending blocking feed item — these get
  // the notification ring. Recomputed reactively as feedItems changes.
  const waitingPaneIds = (): Set<string> => {
    const s = new Set<string>();
    for (const it of feedItems()) {
      if (it.state === "pending" && it.blocking && it.pane_id) s.add(it.pane_id);
    }
    return s;
  };
  // Phase 26: workspace_ids that contain at least one waiting pane —
  // drives the sidebar tab highlight.
  const waitingWorkspaceIds = (): Set<string> => {
    const s = new Set<string>();
    for (const it of feedItems()) {
      if (it.state === "pending" && it.blocking && it.workspace_id) {
        s.add(it.workspace_id);
      }
    }
    return s;
  };

  // Phase 30 → Phase 31: live-update the OS window title from the
  // FOCUSED pane's effective identity (pane override falls back to
  // workspace). With pane-level identity, Yossi can see in Alt+Tab
  // which client he's looking at even when multiple panes from
  // different clients share the same workspace. Format:
  //   "🟣 ClientB ● — winmux"        (focused pane has title/identity)
  //   "🟦 ClientA — winmux"          (no focused pane → workspace fallback)
  // The ● appears when any pane in the active workspace is waiting
  // (cmux-style dirty indicator on the window itself).
  createEffect(() => {
    const ws = activeWs();
    if (!ws) {
      void getCurrentWindow().setTitle("winmux");
      return;
    }
    const parts: string[] = [];
    const pid = activePaneId();
    const focused = pid && ws.layout ? findPane(ws.layout, pid) : null;
    const ident = effectiveIdentity(focused ?? undefined, ws);
    if (ident.emoji) parts.push(ident.emoji);
    const focusedName =
      focused?.title ||
      (focused?.connection ? describeConnection(focused.connection) : null);
    parts.push(focusedName ?? ws.name);
    if (waitingWorkspaceIds().has(ws.id)) parts.push("●");
    const title = parts.join(" ") + " — winmux";
    void getCurrentWindow().setTitle(title);
  });

  // Phase 41: when the user activates an SSH workspace and the setting is
  // on (default), establish a background SSH session so the tmux picker and
  // file manager populate without opening a terminal pane. Fire-and-forget;
  // the backend command is idempotent and skips password-mode workspaces.
  // The id guard fires once per workspace switch (the effect otherwise
  // re-runs on every file() change). We do NOT consume the workspace while
  // settings is still loading, so the initial workspace still auto-connects
  // once settings arrives.
  let lastAutoConnectWs: string | null = null;
  createEffect(() => {
    const ws = activeWs();
    const s = settings();
    if (!ws) {
      lastAutoConnectWs = null;
      return;
    }
    if (!s) return;
    if (ws.id === lastAutoConnectWs) return;
    lastAutoConnectWs = ws.id;
    if (s.auto_connect_on_workspace_select === false) return;
    if (ws.connection?.type !== "ssh") return;
    void invoke("workspace_ensure_connected", { workspaceId: ws.id }).catch((e) =>
      console.warn("workspace_ensure_connected failed", e),
    );
  });

  // Phase 47: on workspace activation, if it's SSH and detection is on,
  // make sure the remote port-watcher is running for this workspace AND
  // replay the current detected_ports snapshot from the backend. Events
  // alone don't fill the FE signal when the workspace was previously
  // active in another session — the detected_ports state may exist on
  // the backend without the FE having seen its events.
  // Phase 47 → 62.C: ensure the remote port-watcher is running and pull
  // a fresh snapshot of detected ports into the FE signal. Extracted from
  // the workspace-activation effect so the Browser window (item C) can
  // call it on open / Refresh too — the Browser needs the port list even
  // when auto_port_forward is off (it forwards on demand per chosen port).
  const ensurePortsSnapshot = (wsId: string) => {
    void invoke("workspace_ensure_port_watcher", { workspaceId: wsId }).catch((e) =>
      console.warn("workspace_ensure_port_watcher failed", e),
    );
    void invoke<{ remote_port: number; addr: string; family: string }[]>(
      "list_detected_ports",
      { workspaceId: wsId },
    )
      .then((snapshot) => {
        setDetectedPorts((prev) => {
          // Replace this workspace's entries with the backend snapshot.
          const other = prev.filter((p) => p.workspace_id !== wsId);
          const mine = snapshot.map((d) => ({
            workspace_id: wsId,
            remote_port: d.remote_port,
            addr: d.addr,
            family: d.family,
          }));
          return [...other, ...mine];
        });
      })
      .catch((e) => console.warn("list_detected_ports failed", e));
  };

  let lastPortsEnsuredWs: string | null = null;
  createEffect(() => {
    const ws = activeWs();
    if (!ws) {
      lastPortsEnsuredWs = null;
      return;
    }
    // Re-fire when the workspace itself changes OR its toggle flips on
    // (so flipping the toggle "live" also kicks the watcher).
    const key = `${ws.id}:${ws.auto_port_forward ? 1 : 0}`;
    if (key === lastPortsEnsuredWs) return;
    lastPortsEnsuredWs = key;
    if (ws.connection?.type !== "ssh") return;
    if (!ws.auto_port_forward) return;
    ensurePortsSnapshot(ws.id);
  });

  const reconcilePanes = (file: WorkspacesFile) => {
    const live = new Set<string>();
    for (const ws of file.workspaces) {
      if (ws.layout) for (const p of collectPanes(ws.layout)) live.add(p);
    }
    for (const [pid, ti] of [...terms]) {
      if (!live.has(pid)) {
        const sid = paneToSession.get(pid);
        if (sid) {
          sessionToPane.delete(sid);
          paneToSession.delete(pid);
        }
        ti.dispose();
        terms.delete(pid);
      }
    }
  };

  const updateFile = (f: WorkspacesFile) => {
    setFile(f);
    reconcilePanes(f);
    bump();
  };

  // ─── workspace mutations ────────────────────────────────────────────────

  const handleCreate = async (input: {
    name: string;
    connection: Connection;
    color?: string;
    cwd?: string;
    setup_command?: string;
    teardown_command?: string;
    env?: EnvVar[];
  }) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_create", { input });
      updateFile(f);
    } catch (e) {
      console.error("workspace_create failed", e);
    }
  };

  const handleUpdate = async (
    id: string,
    fields: {
      name?: string;
      color?: string;
      cwd?: string;
      setup_command?: string;
      teardown_command?: string;
      env?: EnvVar[];
      connection?: Connection;
    }
  ) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_update", {
        workspaceId: id,
        name: fields.name,
        color: fields.color,
        cwd: fields.cwd,
        setupCommand: fields.setup_command,
        teardownCommand: fields.teardown_command,
        env: fields.env,
        connection: fields.connection ?? null,
      });
      updateFile(f);
    } catch (e) {
      console.error("workspace_update failed", e);
    }
  };

  const handleRename = async (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws) return;
    const next = window.prompt("Rename workspace", ws.name);
    if (!next || !next.trim()) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_rename", {
        workspaceId: id,
        name: next.trim(),
      });
      updateFile(f);
    } catch (e) {
      console.error(e);
    }
  };

  const handleDelete = async (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws) return;
    if (!window.confirm(`Delete workspace "${ws.name}"?`)) return;
    // Phase 39: extra confirm when the workspace has notes (they'll be
    // deleted with it). Counts notes strictly belonging to this ws —
    // legacy unassigned (null) notes survive the delete.
    const noteCount = notes().filter((n) => n.workspace_id === id).length;
    if (noteCount > 0) {
      if (!window.confirm(t("workspace.delete.notesWarning", { count: noteCount }))) return;
    }
    try {
      const f = await invoke<WorkspacesFile>("workspace_delete", {
        workspaceId: id,
      });
      updateFile(f);
    } catch (e) {
      console.error(e);
    }
  };

  const handleSetActive = async (id: string) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_set_active", {
        workspaceId: id,
      });
      updateFile(f);
      const ws = f.workspaces.find((w) => w.id === id);
      if (ws?.layout) {
        const firstPane = collectPanes(ws.layout)[0];
        if (firstPane) setActivePaneId(firstPane);
      }
    } catch (e) {
      console.error(e);
    }
  };

  // Phase 40: flip auto_port_forward from the Ports window. The command
  // returns the updated workspace; patch it into the file state.
  const handleToggleAutoForward = async (workspaceId: string, enabled: boolean) => {
    try {
      const updated = await invoke<Workspace>("workspace_set_auto_port_forward", {
        workspaceId,
        enabled,
      });
      const f = file();
      updateFile({
        ...f,
        workspaces: f.workspaces.map((w) => (w.id === updated.id ? updated : w)),
      });
    } catch (e) {
      console.error("workspace_set_auto_port_forward failed", e);
    }
  };

  const handleDisconnectWorkspace = async (id: string) => {
    const ws = file().workspaces.find((w) => w.id === id);
    if (!ws?.layout) return;
    for (const paneId of collectPanes(ws.layout)) {
      await disconnectPane(paneId);
    }
  };

  // ─── pane operations ────────────────────────────────────────────────────

  const splitPane = async (
    paneId: string,
    direction: SplitDirection,
    kind: "terminal" | "browser" | "filemanager" | "diff" = "terminal",
    browserUrl?: string
  ) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_split", {
        workspaceId: ws.id,
        paneId,
        direction,
        paneKind: kind,
        browserUrl: browserUrl ?? null,
      });
      updateFile(f);
    } catch (e) {
      console.error("split failed", e);
    }
  };

  const browserNavigate = async (paneId: string, url: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_navigate", {
        workspaceId: ws.id,
        paneId,
        url,
      });
      updateFile(f);
    } catch (e) {
      console.error("browser navigate failed", e);
    }
  };

  const browserGoBack = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_go_back", {
        workspaceId: ws.id,
        paneId,
      });
      updateFile(f);
    } catch (e) {
      console.error("browser go-back failed", e);
    }
  };

  const browserGoHome = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_go_home", {
        workspaceId: ws.id,
        paneId,
      });
      updateFile(f);
    } catch (e) {
      console.error("browser go-home failed", e);
    }
  };

  // Utility: collapse a workspace's layout back to a single terminal pane,
  // useful when you've split a workspace many times and want to start over.
  const handleResetLayout = async (id: string) => {
    if (
      !window.confirm(
        "Reset this workspace to a single terminal pane? All splits and browser panes in this workspace will be removed."
      )
    )
      return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_reset_layout", {
        workspaceId: id,
      });
      updateFile(f);
    } catch (e) {
      console.error("workspace_reset_layout failed", e);
    }
  };

  const browserSetForward = async (paneId: string, forward: boolean) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("pane_browser_set_forward", {
        workspaceId: ws.id,
        paneId,
        forward,
      });
      updateFile(f);
    } catch (e) {
      console.error("browser set-forward failed", e);
    }
  };

  const closePane = async (paneId: string) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_close_pane", {
        workspaceId: ws.id,
        paneId,
      });
      updateFile(f);
    } catch (e) {
      console.error("close failed", e);
    }
  };

  let ratioCommitTimer: number | null = null;
  const setRatio = (splitId: string, ratio: number, commit: boolean) => {
    const ws = activeWs();
    if (!ws || !ws.layout) return;
    // Optimistic local update for instant feedback
    const updated = updateRatioInLayout(ws.layout, splitId, ratio);
    setFile({
      ...file(),
      workspaces: file().workspaces.map((w) =>
        w.id === ws.id ? { ...w, layout: updated } : w
      ),
    });
    // Trigger fit + pty_resize on all panes in this workspace
    queueMicrotask(() => {
      for (const pid of collectPanes(updated)) terms.get(pid)?.fitAndResize();
    });
    if (commit) {
      if (ratioCommitTimer) clearTimeout(ratioCommitTimer);
      invoke("workspace_set_split_ratio", {
        workspaceId: ws.id,
        splitId,
        ratio,
      }).catch(() => {});
    }
  };

  type ConnectOpts = {
    password?: string;
    keyPassphrase?: string;
    acceptUnknownHost?: boolean;
    persistent?: boolean;
    // Phase 12.B Smart Connect.
    mode?: "default" | "tmux" | "plain" | "cmd" | "claude";
    cwdOverride?: string;
    cmd?: string;
    claudeArgs?: string;
    // Phase 23.F: override tmux session name (picker path).
    tmuxSession?: string;
  };

  const connectPane = async (paneId: string, opts: ConnectOpts = {}) => {
    const ws = activeWs();
    if (!ws) return;
    const ti = ensureTerm(paneId);
    // Phase 62.B (item J): tag the terminal with its workspace so an
    // OSC 8 file:// link click knows which remote to SFTP-download from.
    ti.workspaceId = ws.id;
    setStatus(paneId, "connecting…", false);
    try {
      const sessionId = await invoke<string>("pane_connect", {
        workspaceId: ws.id,
        paneId,
        password: opts.password ?? null,
        keyPassphrase: opts.keyPassphrase ?? null,
        acceptUnknownHost: opts.acceptUnknownHost ?? false,
        persistent: opts.persistent ?? false,
        mode: opts.mode ?? null,
        cwdOverride: opts.cwdOverride ?? null,
        cmd: opts.cmd ?? null,
        claudeArgs: opts.claudeArgs ?? null,
        tmuxSessionName: opts.tmuxSession ?? null,
        cols: ti.term.cols || 80,
        rows: ti.term.rows || 24,
      });
      paneToSession.set(paneId, sessionId);
      sessionToPane.set(sessionId, paneId);
      ti.attach(sessionId);
      clearStatus(paneId);
      setPendingPwFor(null);
      setPendingPassphraseFor(null);
      setPendingHostTrust(null);
      bump();
      // Phase 11.A: persistence map refresh (the SshSession was just inserted
      // with its tmux_session field set or unset). Tiny delay so the handler
      // has finished registering.
      setTimeout(() => void refreshPersistence(), 100);
    } catch (e) {
      const msg = String(e);
      // KEY_PASSPHRASE_REQUIRED:<key_path>
      const pasReq = msg.match(/KEY_PASSPHRASE_REQUIRED:(.+)$/);
      if (pasReq) {
        setPendingPassphraseFor({ paneId, keyPath: pasReq[1] });
        setStatus(paneId, "key requires passphrase", false);
        return;
      }
      // KEY_PASSPHRASE_BAD:<key_path>:<inner_err>
      const pasBad = msg.match(/KEY_PASSPHRASE_BAD:([^:]+):/);
      if (pasBad) {
        setPendingPassphraseFor({
          paneId,
          keyPath: pasBad[1],
          bad: true,
        });
        setStatus(paneId, "wrong passphrase, try again", true);
        return;
      }
      // UNKNOWN_HOST:<target>:<key_type>:<fingerprint>
      const unk = msg.match(/UNKNOWN_HOST:([^:]+:\d+):([^:]+):(.+)$/);
      if (unk) {
        setPendingHostTrust({
          paneId,
          target: unk[1],
          keyType: unk[2],
          fingerprint: unk[3],
        });
        setStatus(paneId, "unknown host — confirm fingerprint", false);
        return;
      }
      // HOST_KEY_MISMATCH:<target>:<key_type>:<old_fp>:<new_fp>
      const mis = msg.match(/HOST_KEY_MISMATCH:([^:]+:\d+):([^:]+):([^:]+):(.+)$/);
      if (mis) {
        setPendingHostTrust({
          paneId,
          target: mis[1],
          keyType: mis[2],
          fingerprint: mis[4],
          mismatchOld: mis[3],
        });
        setStatus(paneId, "host key CHANGED — possible MITM!", true);
        return;
      }
      // Otherwise treat as a generic auth failure → password prompt for SSH
      setStatus(paneId, msg, true);
      const pane = findPaneInActiveWs(paneId);
      if (
        pane &&
        pane.connection.type === "ssh" &&
        msg.includes("authentication failed")
      ) {
        setPendingPwFor(paneId);
      }
    }
  };

  const disconnectPane = async (paneId: string) => {
    try {
      await invoke("pane_disconnect", { paneId });
    } catch (e) {
      console.warn("disconnect failed", e);
    }
    const sid = paneToSession.get(paneId);
    if (sid) {
      sessionToPane.delete(sid);
      paneToSession.delete(paneId);
    }
    terms.get(paneId)?.detach();
    bump();
    void refreshPersistence();
  };

  // Phase 11.A: hard-kill the remote tmux session (if any) and disconnect.
  const killSession = async (paneId: string) => {
    try {
      await invoke("pane_kill_session", { paneId });
    } catch (e) {
      console.warn("kill_session failed", e);
    }
    const sid = paneToSession.get(paneId);
    if (sid) {
      sessionToPane.delete(sid);
      paneToSession.delete(paneId);
    }
    terms.get(paneId)?.detach();
    bump();
    void refreshPersistence();
  };

  const findPaneInActiveWs = (paneId: string) => {
    const ws = activeWs();
    if (!ws?.layout) return null;
    const search = (n: LayoutNode): any => {
      if (n.kind === "pane") return n.pane_id === paneId ? n : null;
      return search(n.first) ?? search(n.second);
    };
    return search(ws.layout);
  };

  // Phase 58: push-to-talk start/stop. Lazily constructs the
  // recorder, drives the indicator, and pastes the returned text
  // into the focused terminal pane on success.
  const startPushToTalk = () => {
    const stt = settings()?.stt;
    if (!stt?.enabled) return;
    setSttError(null);
    const rec = makeSttRecorder(stt.backend, stt.language || "auto");
    sttRecorder = rec;
    setSttListening(true);
    rec
      .start()
      .then((text) => {
        if (text && text.length > 0) {
          pasteIntoActiveTerminal(text);
        }
      })
      .catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        setSttError(msg);
        // Auto-clear after 5s so the toast doesn't linger forever.
        setTimeout(() => setSttError(null), 5000);
      })
      .finally(() => {
        sttRecorder = null;
        setSttListening(false);
      });
  };
  const stopPushToTalk = () => {
    if (!sttRecorder) return;
    try {
      sttRecorder.stop();
    } catch (e) {
      console.warn("stt stop failed", e);
    }
  };

  // Phase 55-B → 60: distribute split ratios evenly. Phase 60
  // (smoke-test 4.2) made the reset OPTIMISTIC: apply the 0.5 ratios
  // to the local file() signal immediately, then let the backend
  // persist + return the canonical snapshot. The visual reset is now
  // instant and independent of the invoke round-trip, and if the
  // backend errors the next workspaces:changed refresh reconciles.
  const distributeEvenly = async () => {
    const ws = activeWs();
    if (!ws) return;
    // Optimistic local pass — walk the layout, reset every ratio.
    const resetRatios = (n: LayoutNode): LayoutNode =>
      n.kind === "split"
        ? { ...n, ratio: 0.5, first: resetRatios(n.first), second: resetRatios(n.second) }
        : n;
    if (ws.layout) {
      const updated = resetRatios(ws.layout);
      setFile({
        ...file(),
        workspaces: file().workspaces.map((w) =>
          w.id === ws.id ? { ...w, layout: updated } : w,
        ),
      });
      queueMicrotask(() => {
        for (const pid of collectPanes(updated)) {
          terms.get(pid)?.fitAndResize();
        }
      });
    }
    try {
      const f = await invoke<WorkspacesFile>("workspace_distribute_evenly", {
        workspaceId: ws.id,
      });
      updateFile(f);
    } catch (e) {
      console.error("workspace_distribute_evenly failed", e);
    }
  };

  // Phase 55-A: maximize toggle. Setting/clearing the signal swaps
  // LayoutView's `node` between the full split tree and the lone
  // leaf; fit+resize fires for every pane in the workspace after the
  // signal flips so xterm catches up to the new available area.
  const toggleMaximize = (paneId?: string) => {
    const cur = maximizedPaneId();
    if (cur) {
      setMaximizedPaneId(null);
    } else {
      const target = paneId ?? activePaneId();
      if (!target) return;
      setMaximizedPaneId(target);
    }
    queueMicrotask(() => {
      const ws = activeWs();
      if (!ws?.layout) return;
      for (const pid of collectPanes(ws.layout)) {
        terms.get(pid)?.fitAndResize();
      }
    });
  };

  // ─── keyboard shortcuts ─────────────────────────────────────────────────

  const handleKey = (e: KeyboardEvent) => {
    // Phase 55-A: Ctrl+Enter toggles maximize for the active pane.
    // Esc restores ONLY when something is maximized (otherwise we
    // step on terminal escape sequences). Hardcoded (not in the
    // shortcut table) — tmux uses Ctrl+b z for the same gesture, but
    // raw Ctrl+Enter is a winmux-specific convenience.
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === "Enter") {
      e.preventDefault();
      toggleMaximize();
      return;
    }
    if (e.key === "Escape" && maximizedPaneId()) {
      e.preventDefault();
      toggleMaximize();
      return;
    }
    // Phase 16: configurable shortcuts. The static Ctrl+Shift+D / E /
    // W bindings (split right / split down / close pane) remain
    // hardcoded for now — they're pane-relative and bound to the
    // active pane, not a global "action", so they don't fit the
    // shortcut-table model. Everything else flows through the table.
    // Phase 35 (#1.3): Ctrl+Shift+P opens the command palette. Hardcoded
    // (not in the shortcut table) — it's a global app action.
    if (e.ctrlKey && e.shiftKey && keyEq(e, "p")) {
      e.preventDefault();
      setShowPalette((v) => !v);
      return;
    }
    // Phase 62.B (item I): Ctrl+B cycles the sidebar mode (full → icons
    // → hidden → full), VS Code-style. Crucially, do NOT steal it when a
    // terminal is focused — Ctrl+b is tmux's prefix and must reach the
    // PTY. Only act when focus is outside an xterm container.
    if (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && keyEq(e, "b")) {
      const inTerminal = (e.target as HTMLElement | null)?.closest?.(
        ".terminal-container",
      );
      if (!inTerminal) {
        e.preventDefault();
        cycleSidebarMode();
        return;
      }
    }
    const sc = shortcutTable();
    if (matches(e, sc.toggle_notes)) {
      e.preventDefault();
      setShowNotes((v) => !v);
      return;
    }
    if (matches(e, sc.toggle_settings)) {
      e.preventDefault();
      setShowSettings((v) => !v);
      return;
    }
    if (matches(e, sc.new_workspace)) {
      e.preventDefault();
      setShowCreate(true);
      return;
    }
    if (matches(e, sc.copy)) {
      // Try the focused terminal first; if it has a selection, copy.
      // Otherwise let the browser handle the event (which may be a
      // text-selection copy in a non-terminal pane).
      void copyTerminalSelection().then((handled) => {
        if (handled) e.preventDefault();
      });
      return;
    }
    if (matches(e, sc.paste)) {
      e.preventDefault();
      navigator.clipboard.readText().then((text) => {
        if (text) pasteIntoActiveTerminal(text);
      }).catch((err) => console.warn("paste failed", err));
      return;
    }
    // Phase 17: Claude session summary.
    if (matches(e, sc.summarize_claude)) {
      e.preventDefault();
      void summarizeActivePane();
      return;
    }
    // Phase 58: push-to-talk (down). Parses the hotkey out of the
    // user's SttSettings on every press — cheap and lets the
    // settings edit take effect without a relaunch. Repeats are
    // suppressed by the sttRecorder guard.
    {
      const stt = settings()?.stt;
      if (stt?.enabled) {
        const accel = parseShortcut(stt.push_to_talk_hotkey);
        if (accel && matches(e, accel) && !sttRecorder) {
          e.preventDefault();
          startPushToTalk();
          return;
        }
      }
    }
    // Phase 55-B → 60 (smoke-test 4.2): Ctrl+Alt+= → distribute
    // splits evenly. The original check matched e.key only — on a
    // Hebrew layout Ctrl+Alt is AltGr and e.key can come back as
    // something other than "="/"+" depending on the compose state.
    // e.code === "Equal" identifies the PHYSICAL key independent of
    // layout, so the shortcut now fires on any keyboard.
    if (
      e.ctrlKey &&
      e.altKey &&
      (e.key === "=" || e.key === "+" || e.code === "Equal")
    ) {
      e.preventDefault();
      void distributeEvenly();
      return;
    }
    // Phase 48-E: Ctrl+Alt+Arrow — split-or-move. Focus the neighbor
    // in that direction if one exists, else split the current pane in
    // that direction.
    if (e.ctrlKey && e.altKey && !e.shiftKey) {
      const dirKey: "left" | "right" | "up" | "down" | null =
        e.key === "ArrowLeft" ? "left"
        : e.key === "ArrowRight" ? "right"
        : e.key === "ArrowUp" ? "up"
        : e.key === "ArrowDown" ? "down"
        : null;
      if (dirKey) {
        e.preventDefault();
        splitOrMove(dirKey);
        return;
      }
      // Phase 49-D: Ctrl+Alt+I/O/K/L → land the active pane in a
      // quadrant. From a single pane: vertical split + horizontal
      // split puts the current pane in one of the four corners. From
      // an existing layout: navigates two split-or-move hops in the
      // corner's direction pair. The 50-50 split convention means the
      // result is approximate — good enough for the common 1-pane and
      // 2-pane starts; complex layouts may land off-corner.
      // Phase 62.B (item G): keyEq → physical-key match so I/O/K/L fire
      // on a Hebrew layout too.
      const qkey: "topLeft" | "topRight" | "bottomLeft" | "bottomRight" | null =
        keyEq(e, "i") ? "topLeft"
        : keyEq(e, "o") ? "topRight"
        : keyEq(e, "k") ? "bottomLeft"
        : keyEq(e, "l") ? "bottomRight"
        : null;
      if (qkey) {
        e.preventDefault();
        const v = qkey.startsWith("top") ? "up" : "down";
        const h = qkey.endsWith("Left") ? "left" : "right";
        splitOrMove(v);
        // Tiny delay so the first split's layout update lands in
        // file() before the second hop reads it. setTimeout(0) is
        // enough for Solid's reactive batch + the Tauri round-trip.
        setTimeout(() => splitOrMove(h), 0);
        return;
      }
    }
    // Pane-relative legacy shortcuts (split / close) still on
    // Ctrl+Shift+D/E/W until we expand the table.
    if (!e.ctrlKey || !e.shiftKey) return;
    const target = activePaneId();
    if (!target) return;
    // Phase 62.B (item G): keyEq → physical-key match (Hebrew layout).
    if (keyEq(e, "d")) {
      e.preventDefault();
      splitPane(target, "horizontal");
    } else if (keyEq(e, "e")) {
      e.preventDefault();
      splitPane(target, "vertical");
    } else if (keyEq(e, "w")) {
      e.preventDefault();
      closePane(target);
    }
  };

  // ─── lifecycle ──────────────────────────────────────────────────────────

  const refreshFromBackend = async () => {
    try {
      const prevActive = file().active_workspace_id;
      const f = await invoke<WorkspacesFile>("workspaces_load");
      updateFile(f);
      // If active workspace changed externally (e.g. via CLI), pick a pane to focus.
      if (
        f.active_workspace_id &&
        f.active_workspace_id !== prevActive
      ) {
        const ws = f.workspaces.find((w) => w.id === f.active_workspace_id);
        if (ws?.layout) {
          const firstPane = collectPanes(ws.layout)[0];
          if (firstPane) setActivePaneId(firstPane);
        }
      }
    } catch (e) {
      console.error("refreshFromBackend failed", e);
    }
  };

  onMount(async () => {
    // Phase 48-D: lightweight UI-stall instrumentation. A 100ms heartbeat
    // measures actual elapsed vs expected and reports gaps >300ms; a
    // PerformanceObserver on `longtask` reports any single task >200ms.
    // Both go to debug.log via the `diag_log` tauri command so future
    // support tickets can correlate UI jank with backend activity.
    // No cleanup: these run for the app's lifetime.
    {
      const HEARTBEAT_MS = 100;
      const STALL_THRESHOLD_MS = 300;
      const LONGTASK_THRESHOLD_MS = 200;
      let lastTick = performance.now();
      window.setInterval(() => {
        const now = performance.now();
        const gap = now - lastTick;
        lastTick = now;
        if (gap > STALL_THRESHOLD_MS) {
          void invoke("diag_log", {
            level: "warn",
            msg: `UI stall: ${Math.round(gap)}ms (expected ~${HEARTBEAT_MS}ms)`,
          }).catch(() => {});
        }
      }, HEARTBEAT_MS);
      try {
        const obs = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            if (entry.duration > LONGTASK_THRESHOLD_MS) {
              void invoke("diag_log", {
                level: "warn",
                msg: `longtask ${entry.name || "(anon)"} ${Math.round(entry.duration)}ms`,
              }).catch(() => {});
            }
          }
        });
        obs.observe({ entryTypes: ["longtask"] });
      } catch {
        // Some WebView versions don't support the longtask entry type — skip.
      }
    }

    // Phase 9.A: load + apply settings as early as possible so the splash
    // colors don't pop to a different palette on first paint.
    try {
      const s = await loadSettings();
      setSettings(s);
      applyTheme(s);
      applyI18nSettings(s.i18n);
      setShortcutTable(buildShortcutTable(s.shortcuts ?? DEFAULT_SHORTCUTS));
      setCtrlCCopyOnSelect(
        (s.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c,
      );
    } catch (e) {
      console.warn("settings_load failed", e);
    }
    await refreshFromBackend();
    const ws0 = file().workspaces.find((w) => w.id === file().active_workspace_id);
    if (ws0?.layout) {
      const p0 = collectPanes(ws0.layout)[0];
      if (p0) setActivePaneId(p0);
    }

    const unlistens: UnlistenFn[] = [];
    unlistens.push(
      await listen<PtyDataEvent>("pty:data", (e) => {
        const pid = sessionToPane.get(e.payload.session_id);
        if (!pid) return;
        terms.get(pid)?.writeData(e.payload.data);
      })
    );
    unlistens.push(
      await listen<PtyExitEvent>("pty:exit", (e) => {
        const pid = sessionToPane.get(e.payload.session_id);
        if (!pid) return;
        sessionToPane.delete(e.payload.session_id);
        paneToSession.delete(pid);
        const ti = terms.get(pid);
        ti?.notice(
          `[disconnected${e.payload.reason ? ` (${e.payload.reason})` : ""}]`
        );
        ti?.detach();
        bump();
        void refreshPersistence();
      })
    );
    // Initial feed load.
    try {
      const items = await invoke<FeedItem[]>("feed_list");
      // Show most recent first.
      setFeedItems([...items].reverse());
      // Auto-dismiss already-resolved items so we don't show stale verdicts.
      for (const it of items) {
        if (it.state !== "pending") scheduleFeedDismiss(it.request_id);
      }
    } catch (e) {
      console.warn("feed_list failed", e);
    }
    // Phase 6.5 feed events.
    unlistens.push(
      await listen<FeedItem>("feed:item-added", (e) => {
        setFeedItems((prev) => [e.payload, ...prev.filter((i) => i.request_id !== e.payload.request_id)]);
        if (e.payload.state !== "pending") scheduleFeedDismiss(e.payload.request_id);
      })
    );
    unlistens.push(
      await listen<FeedResolvedEvent>("feed:item-resolved", (e) => {
        const verdict = e.payload.decision === "allow" ? "allowed" : e.payload.decision === "deny" ? "denied" : e.payload.decision === "timeout" ? "timedout" : "denied";
        setFeedItems((prev) =>
          prev.map((i) =>
            i.request_id === e.payload.request_id
              ? { ...i, state: verdict as FeedItem["state"] }
              : i
          )
        );
        scheduleFeedDismiss(e.payload.request_id);
      })
    );
    // Phase 35 (#1.2): OSC 9/99/777 terminal notifications. The
    // backend's PTY reader detects the escape sequence and emits this
    // event; we surface it as a passive feed item (same rendering as
    // agent-hook passive items). Universal complement to the
    // Claude-specific hooks — works for cargo, pytest, any tool that
    // prints the escape sequence.
    unlistens.push(
      await listen<{ pane_id: string; title: string; body: string; kind: string }>(
        "osc-notification",
        (e) => {
          const { title, body, kind } = e.payload;
          const hasTitle = title.trim().length > 0;
          const item: FeedItem = {
            request_id:
              (globalThis.crypto?.randomUUID?.() ?? `osc-${Date.now()}-${Math.random()}`),
            kind: "notification",
            subkind: kind,
            pane_id: e.payload.pane_id,
            workspace_id: null,
            title: hasTitle ? title : body,
            summary: hasTitle ? body : "",
            payload: e.payload,
            state: "passive",
            created_ms: Date.now(),
            blocking: false,
          };
          setFeedItems((prev) => [item, ...prev]);
          scheduleFeedDismiss(item.request_id);
        },
      ),
    );
    // Phase 36 (#2.2): auto port-forward lifecycle. The backend opens a
    // local SSH forward when the remote watcher reports a new listening
    // port, and emits these events. We track them for the Ports panel
    // Phase 46: ports are DETECTED on remote LISTEN, but a forward is
    // only opened on user click. No FeedItem on either event — the
    // PortsWindow is the only surface. Events:
    //   port-detected      → add to detectedPorts
    //   port-undetected    → remove from detectedPorts (also cleans
    //                         forwards if the port was tunneled)
    //   port-forwarded     → add to portForwards
    //   port-forward-stopped → remove from portForwards
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number; addr: string; family: string }>(
        "port-detected",
        (e) => {
          setDetectedPorts((prev) => [
            ...prev.filter(
              (p) => !(p.workspace_id === e.payload.workspace_id && p.remote_port === e.payload.remote_port),
            ),
            {
              workspace_id: e.payload.workspace_id,
              remote_port: e.payload.remote_port,
              addr: e.payload.addr,
              family: e.payload.family,
            },
          ]);
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number }>(
        "port-undetected",
        (e) => {
          setDetectedPorts((prev) =>
            prev.filter(
              (p) => !(p.workspace_id === e.payload.workspace_id && p.remote_port === e.payload.remote_port),
            ),
          );
        },
      ),
    );
    // Phase 47: detection toggled off → wipe the workspace's entries.
    unlistens.push(
      await listen<{ workspace_id: string }>(
        "port-detection-cleared",
        (e) => {
          setDetectedPorts((prev) =>
            prev.filter((p) => p.workspace_id !== e.payload.workspace_id),
          );
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_addr: string; remote_port: number; local_port: number }>(
        "port-forwarded",
        (e) => {
          const row: ForwardRow = {
            workspace_id: e.payload.workspace_id,
            remote_port: e.payload.remote_port,
            local_port: e.payload.local_port,
            remote_addr: e.payload.remote_addr,
            opened_at: Date.now(),
          };
          setPortForwards((prev) => [
            ...prev.filter(
              (f) => !(f.workspace_id === row.workspace_id && f.remote_port === row.remote_port),
            ),
            row,
          ]);
        },
      ),
    );
    unlistens.push(
      await listen<{ workspace_id: string; remote_port: number }>(
        "port-forward-stopped",
        (e) => {
          setPortForwards((prev) =>
            prev.filter(
              (f) =>
                !(
                  f.workspace_id === e.payload.workspace_id &&
                  f.remote_port === e.payload.remote_port
                ),
            ),
          );
        },
      ),
    );
    // Phase 7.B: notes
    await refreshNotes();
    unlistens.push(
      await listen("notes:changed", () => {
        void refreshNotes();
      })
    );
    // Per-pane status events (e.g. remote-bootstrap progress).
    unlistens.push(
      await listen<{ pane_id: string; text: string }>("pane:status", (e) => {
        const next = { ...paneStatusText() };
        if (e.payload.text) {
          next[e.payload.pane_id] = e.payload.text;
        } else {
          delete next[e.payload.pane_id];
        }
        setPaneStatusText(next);
      })
    );
    // Live refresh when an external mutation happens (RPC over named pipe).
    unlistens.push(
      await listen("workspaces:changed", () => {
        void refreshFromBackend();
      })
    );
    // Phase 9.A: settings updated externally (CLI / RPC) — re-apply theme.
    unlistens.push(
      await listen<Settings>("settings:changed", (e) => {
        setSettings(e.payload);
        applyTheme(e.payload);
        applyI18nSettings(e.payload.i18n);
        setShortcutTable(
          buildShortcutTable(e.payload.shortcuts ?? DEFAULT_SHORTCUTS),
        );
        setCtrlCCopyOnSelect(
          (e.payload.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c,
        );
      })
    );
    // Phase 18: agent-hooks outdated event from the backend's
    // post-bootstrap probe. Surface the banner once per connection.
    unlistens.push(
      await listen<HooksOutdatedInfo>("hooks:outdated", (e) => {
        setHooksBanner(e.payload);
      })
    );

    // Phase 9.B: update available — show a banner; user clicks to open notes.
    unlistens.push(
      await listen<UpdateInfo>("update:available", (e) => {
        setUpdateBanner(e.payload);
      })
    );

    window.addEventListener("keydown", handleKey);
    // Phase 58: keyup half of push-to-talk. We register a generic
    // keyup that stops the active recorder regardless of which
    // modifier was released — typical PTT UX is "any release ends
    // the capture", and trying to match the exact hotkey on keyup
    // misses the very-common case where the user releases Shift
    // before M.
    const handleKeyUp = (_e: KeyboardEvent) => {
      if (sttRecorder) {
        stopPushToTalk();
      }
    };
    window.addEventListener("keyup", handleKeyUp);
    // Phase 55-A: PaneView dispatches a custom event on content
    // double-click (skipping xterm + the header). We listen at the
    // App level so the toggle stays co-located with the maximized
    // signal + the post-toggle fit/resize fanout.
    const handlePaneMaximize = (e: Event) => {
      const detail = (e as CustomEvent).detail as { paneId?: string };
      if (detail?.paneId) toggleMaximize(detail.paneId);
    };
    window.addEventListener("winmux:pane-maximize", handlePaneMaximize);

    // Phase 62.B (item J): a terminal OSC 8 file:// link was clicked.
    // SFTP-download it to the user's Downloads folder, with toasts.
    const handleOscFileLink = (e: Event) => {
      const detail = (e as CustomEvent).detail as {
        workspaceId: string | null;
        path: string;
      } | null;
      if (!detail) return;
      const name = detail.path.split("/").filter(Boolean).pop() || detail.path;
      if (!detail.workspaceId) {
        flashSummaryToast("err", t("osc.download.noRemote"));
        return;
      }
      flashSummaryToast("ok", t("osc.download.started", { name }));
      invoke<string>("download_remote_file_via_osc", {
        workspaceId: detail.workspaceId,
        remotePath: detail.path,
      })
        .then((local) => flashSummaryToast("ok", t("osc.download.done", { path: local })))
        .catch((err) =>
          flashSummaryToast("err", t("osc.download.failed", { msg: String(err) })),
        );
    };
    window.addEventListener("winmux:osc-file-link", handleOscFileLink);

    onCleanup(() => {
      for (const u of unlistens) u();
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("keyup", handleKeyUp);
      window.removeEventListener("winmux:pane-maximize", handlePaneMaximize);
      window.removeEventListener("winmux:osc-file-link", handleOscFileLink);
      for (const [pid] of paneToSession) {
        invoke("pane_disconnect", { paneId: pid }).catch(() => {});
      }
      for (const [, ti] of terms) ti.dispose();
      terms.clear();
    });
  });

  return (
    <div
      class="app"
      style={{ "grid-template-columns": `${sidebarPx()}px 1fr` }}
    >
      <ErrorBoundary
        fallback={(err) => (
          <div class="sidebar-error">
            <p>{t("error.sidebarRender")}</p>
            <pre>{String(err)}</pre>
            <button class="primary" onClick={() => setShowCreate(true)}>
              + New workspace
            </button>
          </div>
        )}
      >
        <Sidebar
          workspaces={file().workspaces}
          activeId={file().active_workspace_id}
          connectedIds={liveWorkspaceIds()}
          waitingWorkspaceIds={waitingWorkspaceIds()}
          onActivate={handleSetActive}
          onCreate={() => setShowCreate(true)}
          onProvision={() => setShowProvision(true)}
          onConnectExisting={() => setConnectExisting({})}
          onOpenSettings={() => setShowSettings(true)}
          onOpenNotes={() => setShowNotes(true)}
          onAction={(id, action) => {
            if (action === "rename") handleRename(id);
            else if (action === "edit") {
              const ws = file().workspaces.find((w) => w.id === id);
              if (ws) {
                setEditingWorkspace(ws);
                setShowCreate(true);
              }
            } else if (action === "delete") void handleDelete(id);
            else if (action === "disconnect")
              void handleDisconnectWorkspace(id);
            // Phase 65.Q removed the "add_machine" action — joining an
            // existing server is handled by the main wizard (R).
          }}
          allForwards={portForwards()}
          onOpenPorts={(workspaceId) => {
            // Badge click: activate that workspace, then open the
            // (active-workspace-scoped) Ports window.
            void handleSetActive(workspaceId);
            setShowPortsWindow(true);
          }}
          onOpenPortsGlobal={() => setShowPortsWindow(true)}
          mode={sidebarMode()}
          onSetMode={setSidebarMode}
        />
      </ErrorBoundary>
      {/* Phase 62.B (item I): drag handle on the sidebar/main boundary —
          only in full mode (icons is a fixed width). Phase 65.P removed
          the "hidden" mode and its edge reopen-tab. */}
      <Show when={sidebarMode() === "full"}>
        <div
          class="sidebar-resizer"
          style={{ "inset-inline-start": `${sidebarPx()}px` }}
          onMouseDown={startSidebarResize}
          title={t("sidebar.resize.tooltip")}
        />
      </Show>
      <div class="main">
        {/* Phase 30: per-workspace accent strip. Sets the CSS variable
            inline so the rule in App.css can paint it without needing a
            second class per workspace. Hidden via data-empty when the
            workspace has no color (or no active workspace at all). */}
        <div
          class="ws-accent-strip"
          data-empty={activeWs()?.color ? "false" : "true"}
          style={activeWs()?.color ? `--ws-color: ${activeWs()!.color}` : undefined}
        />
        <Show when={activeWs()}>
          <ErrorBoundary
            fallback={(err) => (
              <div class="ws-header layout-error">
                <span class="ws-title">{activeWs()?.name ?? "(unknown)"}</span>
                <span class="ws-conn-info">{String(err)}</span>
                <button
                  class="ws-header-btn"
                  onClick={() => handleResetLayout(activeWs()!.id)}
                >
                  Reset to single pane
                </button>
              </div>
            )}
          >
          <div class="ws-header">
            <span
              class="ws-dot"
              style={{ background: activeWs()!.color || "#6b7682" }}
            />
            <span class="ws-title">{activeWs()!.name}</span>
            <Show when={activeWs()!.layout?.kind === "pane"}>
              <span class="ws-conn-info">
                {(() => {
                  const layout = activeWs()!.layout as Extract<
                    LayoutNode,
                    { kind: "pane" }
                  >;
                  if (layout.pane_kind === "browser") return "browser";
                  return layout.connection
                    ? describeConnection(layout.connection)
                    : "—";
                })()}
              </span>
            </Show>
            <Show when={activeWs()!.layout?.kind === "split"}>
              <span class="ws-conn-info">
                {collectPanes(activeWs()!.layout!).length} panes
              </span>
            </Show>
            <Show when={activeWs()!.layout && activePaneId()}>
              {/* Phase 50: add a Diff pane (#2.4). Same split mechanic
                  as the other kinds. */}
              <button
                class="ws-header-btn"
                title={t("ws_header.split_diff_title")}
                onClick={() => {
                  const pid = activePaneId();
                  if (pid) splitPane(pid, "horizontal", "diff");
                }}
              >
                {t("ws_header.add_diff")}
              </button>
              {/* Phase 60 (smoke-test 2a): Browser + Files buttons
                  live HERE, next to + diff — they're workspace-scoped
                  tools, so they belong in the workspace header, not
                  in the global sidebar. The i18n keys keep their
                  historical "sidebar." prefix; renaming 8 keys × 4
                  locales for a cosmetic prefix isn't worth the churn. */}
              <button
                class="ws-header-btn"
                title={t("sidebar.browser.tooltip")}
                onClick={() => setShowBrowserWindow(true)}
              >
                🌐 {t("sidebar.browser.label")}
              </button>
              <button
                class="ws-header-btn"
                title={t("sidebar.files.tooltip")}
                onClick={() => setShowFilesWindow(true)}
              >
                🗂 {t("sidebar.files.label")}
              </button>
              {/* Phase 24.D: removed + chat / + claude log buttons.
                  The two pane kinds + their backends are rolled back
                  pending a future unified-view rebuild. */}
            </Show>
          </div>
          </ErrorBoundary>
        </Show>

        <Show when={!activeWs()}>
          <div class="empty">
            <p>{t("ws.empty.none")}</p>
            <button class="primary" onClick={() => setShowCreate(true)}>
              {t("ws.empty.new")}
            </button>
          </div>
        </Show>

        <Show when={activeWs()?.layout}>
          {/* Phase 62.B (item H): workspace color frames the whole pane
              area (outer border). Pane colors frame each pane inside. */}
          <div
            class="layout-root"
            data-has-color={activeWs()?.color ? "true" : "false"}
            style={activeWs()?.color ? `--ws-color: ${activeWs()!.color}` : undefined}
          >
            {/* Phase 8 fix v3: ErrorBoundary so a single corrupted workspace
                layout (e.g. from the recent autosave-loop nesting) doesn't
                blank the whole app. Falls back to a clear reset button. */}
            <ErrorBoundary
              fallback={(err, _reset) => (
                <div class="layout-error">
                  <p>{t("error.layoutRender")}</p>
                  <pre class="layout-error-detail">{String(err)}</pre>
                  <button
                    class="primary"
                    onClick={() => handleResetLayout(activeWs()!.id)}
                  >
                    Reset to single pane
                  </button>
                </div>
              )}
            >
              {/* Phase 28: keyed Show on workspace id. Switching
                  workspaces (id changes) tears down the LayoutView
                  subtree so PaneView's onMount re-runs and attaches
                  the correct terminal container — fixes the
                  "switching workspaces shows the previous workspace's
                  terminal" bug. Layout edits within ONE workspace
                  (split / close pane) keep the same id, so Solid's
                  fine-grained reactivity handles them without a
                  full subtree recreation. Terminal instances live in
                  the g_terminals registry keyed by pane_id, so they
                  survive the DOM detach/reattach with no scrollback
                  or session loss. */}
              <Show when={activeWs()?.id} keyed>
                {(_id) => (
                  <LayoutView
                    workspaceId={activeWs()!.id}
                    node={(() => {
                      const ws = activeWs()!;
                      const max = maximizedPaneId();
                      if (!max) return ws.layout!;
                      // Phase 55-A: when a pane is maximized, swap
                      // the tree for that one leaf so it fills the
                      // workspace area. Splits + the other panes are
                      // still in `ws.layout` so restore brings them
                      // straight back without re-creating any
                      // TerminalInstance (those are keyed by pane_id
                      // in the `terms` map, surviving the DOM detach).
                      const node = findPane(ws.layout!, max);
                      return node ?? ws.layout!;
                    })()}
                    activePaneId={activePaneId()}
                    connectedPaneIds={connectedPanes()}
                    waitingPaneIds={waitingPaneIds()}
                    workspaceConnection={activeWs()?.connection ?? undefined}
                    workspaceName={activeWs()?.name}
                    workspaceColor={activeWs()?.color ?? undefined}
                    workspaceEmoji={activeWs()?.emoji ?? undefined}
                    workspaceIsSsh={
                      // Phase 16: walk the active workspace's layout looking for
                      // any pane with an SSH connection. We pre-compute this
                      // here so FileManagerPane (which lives deeper in the
                      // tree and has no connection of its own) can render the
                      // remote column even before the user opens a terminal.
                      (() => {
                        const ws = activeWs();
                        if (!ws) return false;
                        if (ws.connection?.type === "ssh") return true;
                        const walk = (n: LayoutNode): boolean => {
                          if (n.kind === "pane") {
                            return n.connection?.type === "ssh";
                          }
                          return walk(n.first) || walk(n.second);
                        };
                        return ws.layout ? walk(ws.layout) : false;
                      })()
                    }
                    pendingPasswordFor={pendingPwFor()}
                    pendingPassphrase={pendingPassphraseFor()}
                    pendingHostTrust={pendingHostTrust()}
                    paneStatus={paneStatus()}
                    paneStatusText={paneStatusText()}
                    panePersistence={panePersistence()}
                    ensureTerm={ensureTerm}
                    onFocus={(pid) => {
                      setActivePaneId(pid);
                      terms.get(pid)?.focus();
                    }}
                    onConnect={(pid, opts) => connectPane(pid, opts)}
                    onSplit={splitPane}
                    onClose={closePane}
                    onDisconnect={disconnectPane}
                    onKillSession={killSession}
                    onSetTitle={(pid, title) => {
                      const ws = activeWs();
                      if (!ws) return;
                      invoke<WorkspacesFile>("pane_set_title", {
                        workspaceId: ws.id,
                        paneId: pid,
                        title: title.trim() === "" ? null : title,
                      })
                        .then((f) => updateFile(f))
                        .catch((e) => console.error("pane_set_title failed", e));
                    }}
                    onSetAnnotation={(pid, annotation) => {
                      const ws = activeWs();
                      if (!ws) return;
                      invoke<WorkspacesFile>("pane_set_annotation", {
                        workspaceId: ws.id,
                        paneId: pid,
                        annotation:
                          annotation.trim() === "" ? null : annotation,
                      })
                        .then((f) => updateFile(f))
                        .catch((e) =>
                          console.error("pane_set_annotation failed", e)
                        );
                    }}
                    onRatioDrag={(sid, r) => setRatio(sid, r, false)}
                    onRatioCommit={(sid, r) => setRatio(sid, r, true)}
                    onBrowserNavigate={browserNavigate}
                    onBrowserGoBack={browserGoBack}
                    onBrowserGoHome={browserGoHome}
                    onBrowserSetForward={browserSetForward}
                  />
                )}
              </Show>
            </ErrorBoundary>
          </div>
        </Show>
      </div>

      <CreateWorkspaceModal
        open={showCreate()}
        editing={editingWorkspace()}
        onClose={() => {
          setShowCreate(false);
          setEditingWorkspace(null);
        }}
        onCreate={handleCreate}
        onUpdate={handleUpdate}
        onOpenSshHelp={() => {
          // Phase 34: split a Help pane off the currently-active
          // workspace's focused pane. No-op when no workspace exists
          // (fresh-launch state) — Yossi can extend later if the
          // common case becomes "from-the-create-modal with nothing
          // open yet".
          const ws = activeWs();
          const pid = activePaneId();
          if (!ws || !pid) return;
          void invoke("workspace_split", {
            workspaceId: ws.id,
            paneId: pid,
            direction: "horizontal",
            paneKind: "help",
            browserUrl: null,
            helpTopic: "ssh-key-setup",
          });
        }}
      />

      {/* Phase 47.E: removed the floating Notes (📝 N) and Settings (⚙)
          FABs from the workspace area — duplicates of the sidebar bottom
          row [📝 Notes][⚙ Settings][🌐 Ports] added in Phase 39 (re-added
          in Phase 40). The Ctrl+Shift+N keyboard shortcut for Notes
          stays wired separately. */}
      {/* Phase 56-A: keyed Show forces ProvisioningWizard to fully
          unmount on close + freshly remount on re-open. Without this,
          the component instance lives across opens and its internal
          signals (wizStep, host, port, runId, …) stick — so clicking
          "Provision server" after a completion screen reopens to that
          completion. The keyed flag is the explicit hint that we're
          using the component as a transient session, not as a
          persistent always-mounted modal. */}
      <Show keyed when={showProvision()}>
        <ProvisioningWizard
          open={true}
          onClose={() => setShowProvision(false)}
          onOpenWorkspace={async (wsId, mode) => {
            // Phase 14.A.2: the wizard's backend already emitted
            // `workspaces:changed` when it created/updated the
            // workspace, so by the time we land here our local state
            // already shows the new entry. Switch to it + auto-connect
            // the first pane.
            try {
              await handleSetActive(wsId);
              const ws = file().workspaces.find((w) => w.id === wsId);
              const firstPane =
                ws?.layout ? collectPanes(ws.layout)[0] : null;
              if (firstPane) {
                setActivePaneId(firstPane);
                connectPane(firstPane, {
                  persistent: true,
                  ...(mode === "claude" ? { mode: "claude" } : {}),
                });
              }
            } catch (e) {
              console.error("open created workspace failed", e);
            }
          }}
        />
      </Show>

      {/* Phase 65.C: Connect-to-existing-server wizard. onCreated mirrors
          the provisioning flow — activate the workspace + connect its
          first pane. */}
      <ConnectExistingWizard
        open={connectExisting() !== null}
        existingWorkspaceId={connectExisting()?.existingWorkspaceId}
        prefill={connectExisting()?.prefill}
        onClose={() => setConnectExisting(null)}
        onCreated={async (wsId) => {
          try {
            await handleSetActive(wsId);
            const ws = file().workspaces.find((w) => w.id === wsId);
            const firstPane = ws?.layout ? collectPanes(ws.layout)[0] : null;
            if (firstPane) {
              setActivePaneId(firstPane);
              connectPane(firstPane, { persistent: true });
            }
          } catch (e) {
            console.error("open connect-existing workspace failed", e);
          }
        }}
      />

      <Show when={settings()}>
        <SettingsModal
          open={showSettings()}
          settings={settings()!}
          onClose={() => setShowSettings(false)}
          onChange={(next) => setSettings(next)}
        />
      </Show>

      {/* Phase 32.B: SSH key offer. Self-contained — listens for the
          `ssh-key-offer` event on its own, no props needed. */}
      <SshKeyOfferModal />

      {/* Phase 35 (#1.3): command palette (Ctrl+Shift+P). */}
      <CommandPalette
        open={showPalette()}
        commands={paletteCommands()}
        onClose={() => setShowPalette(false)}
      />

      {/* Phase 40 → 46: floating Ports window, scoped to the active
          workspace. Detected ports show as click-to-forward; forwarded
          ports show Open/Stop. No FeedItem on either event. */}
      <PortsWindow
        open={showPortsWindow()}
        activeWorkspace={activeWs()}
        detectedPorts={detectedPorts()}
        forwards={portForwards()}
        onClose={() => setShowPortsWindow(false)}
        onStop={stopForward}
        onStart={startForward}
        onToggleAutoForward={handleToggleAutoForward}
      />

      {/* Phase 53 (rebased): floating workspace-level Browser window.
          The native child Webview lives on the Rust side keyed by
          workspace_id; this shell owns the chrome (header, drag, resize,
          persisted geometry). Hide-on-close preserves page state until
          the workspace is deleted. */}
      <BrowserWindow
        open={showBrowserWindow()}
        workspace={activeWs()}
        anyModalOpen={anyModalOpen}
        onClose={() => setShowBrowserWindow(false)}
        detectedPorts={(() => {
          const id = file().active_workspace_id;
          return id
            ? detectedPorts()
                .filter((p) => p.workspace_id === id)
                .map((p) => ({
                  remote_port: p.remote_port,
                  addr: p.addr,
                  family: p.family,
                }))
            : [];
        })()}
        forwards={(() => {
          const id = file().active_workspace_id;
          return id
            ? portForwards()
                .filter((f) => f.workspace_id === id)
                .map((f) => ({
                  remote_port: f.remote_port,
                  local_port: f.local_port,
                }))
            : [];
        })()}
        onEnsurePorts={ensurePortsSnapshot}
        onStartForward={(remotePort) => {
          const id = file().active_workspace_id;
          if (!id) return Promise.reject(new Error("no active workspace"));
          return startForward(id, remotePort);
        }}
      />

      {/* Phase 53 (rebased): floating workspace-level File Manager
          window. Pure HTML chrome around the existing dual-column
          FileManagerPane (local + remote SFTP). hasActiveSession
          drives whether the remote column lights up. */}
      <FileManagerWindow
        open={showFilesWindow()}
        workspace={activeWs()}
        hasActiveSession={(() => {
          const id = file().active_workspace_id;
          return id ? liveWorkspaceIds().has(id) : false;
        })()}
        onClose={() => setShowFilesWindow(false)}
      />

      {/* Phase 58: voice-input recording indicator + error toast.
          Floating top-right, dismissible only by stopping the
          recording (release the PTT key) or letting the 5s timeout
          clear the error. Mutually exclusive in practice — the
          recorder finally{} clears sttListening before sttError
          gets set on the error path. */}
      <Show when={sttListening()}>
        <div class="stt-indicator" role="status">
          <span class="stt-indicator-dot" />
          <span>{t("stt.listening")}</span>
        </div>
      </Show>
      <Show when={sttError()}>
        <div class="stt-indicator stt-indicator-err" role="alert">
          {t("stt.error", { message: sttError()! })}
        </div>
      </Show>

      <Show when={updateBanner()}>
        <div class="update-banner" role="status">
          <div class="update-banner-body">
            <strong>winmux {updateBanner()!.latest_version}</strong>{" "}
            is available — current {updateBanner()!.current_version}.
          </div>
          <div class="update-banner-actions">
            <Show when={updateBanner()!.notes_url}>
              <a
                class="update-banner-link"
                href={updateBanner()!.notes_url ?? "#"}
                target="_blank"
                rel="noopener noreferrer"
              >
                {t("update_banner.notes")}
              </a>
            </Show>
            {/* Phase 27: one-click auto-install. The backend downloads
                the NSIS installer, verifies its sha256 against the
                manifest, runs it, and exits the app. */}
            <button
              class="update-banner-install"
              disabled={installingUpdate()}
              onClick={() => void installUpdate()}
            >
              {installingUpdate()
                ? t("update_banner.installing")
                : t("update_banner.install")}
            </button>
            <button class="update-banner-x" onClick={() => setUpdateBanner(null)}>×</button>
          </div>
        </div>
      </Show>

      <Show when={hooksBanner()}>
        <div class="hooks-banner" role="status">
          <div class="hooks-banner-body">
            <strong>{t("hooks_update.banner.title")}</strong>
            <span class="hooks-banner-detail">
              {t("hooks_update.banner.text", {
                agent: hooksBanner()!.agent,
                current: hooksBanner()!.current ?? "—",
                latest: hooksBanner()!.latest,
              })}
            </span>
          </div>
          <div class="hooks-banner-actions">
            <button
              class="hooks-banner-btn primary"
              disabled={hooksUpdating()}
              onClick={() => void triggerHooksUpdate()}
            >
              {hooksUpdating() ? t("common.saving") : t("hooks_update.btn.update")}
            </button>
            <button class="hooks-banner-btn" onClick={dismissHooksLater}>
              {t("hooks_update.btn.later")}
            </button>
            <button class="hooks-banner-btn" onClick={() => void skipHooksVersion()}>
              {t("hooks_update.btn.skip")}
            </button>
          </div>
        </div>
      </Show>

      <Show when={summaryToast()}>
        <div
          class={`summary-toast ${summaryToast()!.kind}`}
          onClick={() => setSummaryToast(null)}
          role="status"
        >
          <span class="summary-toast-icon">{summaryToast()!.kind === "ok" ? "✓" : "⚠"}</span>
          <span class="summary-toast-text">{summaryToast()!.text}</span>
        </div>
      </Show>

      <NotesModal
        open={showNotes()}
        notes={notes()}
        workspaces={file().workspaces}
        activeWorkspaceId={file().active_workspace_id}
        onClose={() => setShowNotes(false)}
        onAdd={(text, tag, workspaceId) => {
          invoke<Note>("notes_add", {
            text,
            tag: tag ?? null,
            workspaceId: workspaceId ?? null,
            paneId: null,
          })
            .then(() => refreshNotes())
            .catch((e) => console.error("notes_add failed", e));
        }}
        onDone={(id) =>
          invoke("notes_update", { id, status: "done" })
            .then(() => refreshNotes())
            .catch((e) => console.error("notes_update done failed", e))
        }
        onReopen={(id) =>
          invoke("notes_update", { id, status: "open" })
            .then(() => refreshNotes())
            .catch((e) => console.error("notes_update reopen failed", e))
        }
        onDelete={(id) =>
          invoke("notes_delete", { id })
            .then(() => refreshNotes())
            .catch((e) => console.error("notes_delete failed", e))
        }
      />

      <FeedPanel
        items={feedItems()}
        onDecide={(rid, dec) => {
          // Optimistic local update — backend event will reaffirm.
          setFeedItems((prev) =>
            prev.map((i) =>
              i.request_id === rid
                ? { ...i, state: dec === "allow" ? "allowed" : "denied" }
                : i
            )
          );
          invoke("feed_decide", { requestId: rid, decision: dec }).catch(
            (err) => console.error("feed_decide failed", err)
          );
        }}
        onDismiss={(rid) =>
          setFeedItems((prev) => prev.filter((i) => i.request_id !== rid))
        }
      />
    </div>
  );
}

function updateRatioInLayout(
  node: LayoutNode,
  splitId: string,
  ratio: number
): LayoutNode {
  if (node.kind === "pane") return node;
  if (node.split_id === splitId) {
    return { ...node, ratio: Math.max(0.05, Math.min(0.95, ratio)) };
  }
  return {
    ...node,
    first: updateRatioInLayout(node.first, splitId, ratio),
    second: updateRatioInLayout(node.second, splitId, ratio),
  };
}

export default App;
