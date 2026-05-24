import { createSignal, ErrorBoundary, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Sidebar } from "./Sidebar";
import { CreateWorkspaceModal } from "./CreateWorkspaceModal";
import { LayoutView } from "./LayoutView";
import { FeedPanel } from "./FeedPanel";
import { NotesModal } from "./NotesModal";
import { ProvisioningWizard } from "./ProvisioningWizard";
import { SettingsModal } from "./SettingsModal";
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
  type UpdateInfo,
  type HooksOutdatedInfo,
} from "./settings";
import { applyI18nSettings, t } from "./i18n";
import { buildShortcutTable, matches, type ParsedShortcut } from "./shortcuts";
import {
  collectPanes,
  describeConnection,
  type Connection,
  type EnvVar,
  type FeedItem,
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

function App() {
  const [file, setFile] = createSignal<WorkspacesFile>({
    version: 1,
    active_workspace_id: null,
    workspaces: [],
  });
  const [showCreate, setShowCreate] = createSignal(false);
  const [editingWorkspace, setEditingWorkspace] = createSignal<Workspace | null>(null);
  const [activePaneId, setActivePaneId] = createSignal<string | null>(null);
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
  // Phase 18: hooks-outdated banners — at most one banner per agent
  // at a time; the user dismisses (skip-this-version persists), defers
  // (banner gone until next connect), or triggers an in-place update.
  const [hooksBanner, setHooksBanner] = createSignal<HooksOutdatedInfo | null>(null);
  const [hooksUpdating, setHooksUpdating] = createSignal(false);
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
    kind: "terminal" | "browser" | "filemanager" = "terminal",
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

  // ─── keyboard shortcuts ─────────────────────────────────────────────────

  const handleKey = (e: KeyboardEvent) => {
    // Phase 16: configurable shortcuts. The static Ctrl+Shift+D / E /
    // W bindings (split right / split down / close pane) remain
    // hardcoded for now — they're pane-relative and bound to the
    // active pane, not a global "action", so they don't fit the
    // shortcut-table model. Everything else flows through the table.
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
    // Pane-relative legacy shortcuts (split / close) still on
    // Ctrl+Shift+D/E/W until we expand the table.
    if (!e.ctrlKey || !e.shiftKey) return;
    const target = activePaneId();
    if (!target) return;
    if (e.key === "D" || e.key === "d") {
      e.preventDefault();
      splitPane(target, "horizontal");
    } else if (e.key === "E" || e.key === "e") {
      e.preventDefault();
      splitPane(target, "vertical");
    } else if (e.key === "W" || e.key === "w") {
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

    onCleanup(() => {
      for (const u of unlistens) u();
      window.removeEventListener("keydown", handleKey);
      for (const [pid] of paneToSession) {
        invoke("pane_disconnect", { paneId: pid }).catch(() => {});
      }
      for (const [, ti] of terms) ti.dispose();
      terms.clear();
    });
  });

  return (
    <div class="app">
      <ErrorBoundary
        fallback={(err) => (
          <div class="sidebar-error">
            <p>Sidebar failed to render.</p>
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
          }}
        />
      </ErrorBoundary>
      <div class="main">
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
            {/* Phase 8.A: split active pane into a browser pane on the right. */}
            <Show when={activeWs()!.layout && activePaneId()}>
              <button
                class="ws-header-btn"
                title={t("ws_header.split_browser_title")}
                onClick={() => {
                  const pid = activePaneId();
                  if (pid) splitPane(pid, "horizontal", "browser");
                }}
              >
                {t("ws_header.add_browser")}
              </button>
              <button
                class="ws-header-btn"
                title={t("ws_header.split_filemanager_title")}
                onClick={() => {
                  const pid = activePaneId();
                  if (pid) splitPane(pid, "horizontal", "filemanager");
                }}
              >
                {t("ws_header.add_filemanager")}
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
          <div class="layout-root">
            {/* Phase 8 fix v3: ErrorBoundary so a single corrupted workspace
                layout (e.g. from the recent autosave-loop nesting) doesn't
                blank the whole app. Falls back to a clear reset button. */}
            <ErrorBoundary
              fallback={(err, _reset) => (
                <div class="layout-error">
                  <p>Failed to render this layout.</p>
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
                    node={activeWs()!.layout!}
                    activePaneId={activePaneId()}
                    connectedPaneIds={connectedPanes()}
                    waitingPaneIds={waitingPaneIds()}
                    workspaceConnection={activeWs()?.connection}
                    workspaceName={activeWs()?.name}
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
      />

      <button
        class="notes-fab"
        title={`${t("fab.notes")} (Ctrl+Shift+N)`}
        onClick={() => setShowNotes(true)}
      >
        📝 {notes().filter((n) => n.status === "open").length}
      </button>

      <button
        class="settings-fab"
        title={t("fab.settings")}
        onClick={() => setShowSettings(true)}
      >
        ⚙
      </button>

      <ProvisioningWizard
        open={showProvision()}
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

      <Show when={settings()}>
        <SettingsModal
          open={showSettings()}
          settings={settings()!}
          onClose={() => setShowSettings(false)}
          onChange={(next) => setSettings(next)}
        />
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
