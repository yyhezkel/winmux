import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Sidebar } from "./Sidebar";
import { CreateWorkspaceModal } from "./CreateWorkspaceModal";
import { LayoutView } from "./LayoutView";
import { TerminalInstance } from "./terminalInstance";
import {
  collectPanes,
  describeConnection,
  type Connection,
  type LayoutNode,
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
  }) => {
    try {
      const f = await invoke<WorkspacesFile>("workspace_create", { input });
      updateFile(f);
    } catch (e) {
      console.error("workspace_create failed", e);
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

  const splitPane = async (paneId: string, direction: SplitDirection) => {
    const ws = activeWs();
    if (!ws) return;
    try {
      const f = await invoke<WorkspacesFile>("workspace_split", {
        workspaceId: ws.id,
        paneId,
        direction,
      });
      updateFile(f);
    } catch (e) {
      console.error("split failed", e);
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
      <Sidebar
        workspaces={file().workspaces}
        activeId={file().active_workspace_id}
        connectedIds={liveWorkspaceIds()}
        onActivate={handleSetActive}
        onCreate={() => setShowCreate(true)}
        onAction={(id, action) => {
          if (action === "rename") handleRename(id);
          else if (action === "delete") void handleDelete(id);
          else if (action === "disconnect")
            void handleDisconnectWorkspace(id);
        }}
      />
      <div class="main">
        <Show when={activeWs()}>
          <div class="ws-header">
            <span
              class="ws-dot"
              style={{ background: activeWs()!.color || "#6b7682" }}
            />
            <span class="ws-title">{activeWs()!.name}</span>
            <Show when={activeWs()!.layout?.kind === "pane"}>
              <span class="ws-conn-info">
                {describeConnection(
                  (activeWs()!.layout as any).connection
                )}
              </span>
            </Show>
            <Show when={activeWs()!.layout?.kind === "split"}>
              <span class="ws-conn-info">
                {collectPanes(activeWs()!.layout!).length} panes
              </span>
            </Show>
          </div>
        </Show>

        <Show when={!activeWs()}>
          <div class="empty">
            <p>No workspace yet.</p>
            <button class="primary" onClick={() => setShowCreate(true)}>
              + New workspace
            </button>
          </div>
        </Show>

        <Show when={activeWs()?.layout}>
          <div class="layout-root">
            <LayoutView
              workspaceId={activeWs()!.id}
              node={activeWs()!.layout!}
              activePaneId={activePaneId()}
              connectedPaneIds={connectedPanes()}
              pendingPasswordFor={pendingPwFor()}
              pendingPassphrase={pendingPassphraseFor()}
              pendingHostTrust={pendingHostTrust()}
              paneStatus={paneStatus()}
              paneStatusText={paneStatusText()}
              ensureTerm={ensureTerm}
              onFocus={(pid) => {
                setActivePaneId(pid);
                terms.get(pid)?.focus();
              }}
              onConnect={(pid, opts) => connectPane(pid, opts)}
              onSplit={splitPane}
              onClose={closePane}
              onDisconnect={disconnectPane}
              onRatioDrag={(sid, r) => setRatio(sid, r, false)}
              onRatioCommit={(sid, r) => setRatio(sid, r, true)}
            />
          </div>
        </Show>
      </div>

      <CreateWorkspaceModal
        open={showCreate()}
        onClose={() => setShowCreate(false)}
        onCreate={handleCreate}
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
