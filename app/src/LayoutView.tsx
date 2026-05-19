import { Match, Show, Switch } from "solid-js";
import { Divider } from "./Divider";
import { BrowserPane } from "./BrowserPane";
import { FileManagerPane } from "./FileManagerPane";
import { ClaudeChatPane } from "./ClaudeChatPane";
import {
  PaneView,
  type ConnectOpts,
  type HostTrustPending,
  type PassphrasePending,
} from "./PaneView";
import {
  paneKindOf,
  type Connection,
  type LayoutNode,
  type SplitDirection,
  type WorkspacesFile,
} from "./types";
import type { TerminalInstance } from "./terminalInstance";

interface Props {
  workspaceId: string;
  node: LayoutNode;
  activePaneId: string | null;
  connectedPaneIds: Set<string>;
  pendingPasswordFor: string | null;
  pendingPassphrase: PassphrasePending | null;
  pendingHostTrust: HostTrustPending | null;
  paneStatus: Record<string, { msg: string; err: boolean }>;
  paneStatusText: Record<string, string>;
  ensureTerm: (paneId: string) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  // Phase 11.A: tmux session map keyed by pane_id; presence = persistent.
  panePersistence: Record<string, string>;
  onKillSession: (paneId: string) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  onRatioDrag: (splitId: string, ratio: number) => void;
  onRatioCommit: (splitId: string, ratio: number) => void;
  // Phase 8.A: browser-pane callbacks.
  onBrowserNavigate: (paneId: string, url: string) => void;
  onBrowserGoBack: (paneId: string) => void;
  onBrowserGoHome: (paneId: string) => void;
  // Phase 8.B: per-pane forward toggle.
  onBrowserSetForward: (paneId: string, forward: boolean) => void;
  // Phase 16: workspace-level SSH detection. The file manager pane
  // can't tell from its own LayoutNode whether the workspace is SSH
  // (file-manager panes carry no connection), so the parent (App)
  // tells it explicitly. True iff the workspace has at least one
  // pane with an SSH connection.
  workspaceIsSsh: boolean;
  // Phase 23.D: the workspace's canonical connection, threaded
  // through to PaneView so isSsh() can fall back to it.
  workspaceConnection?: Connection;
  /** Phase 22: chat panes need to flush an updated WorkspacesFile back
   *  to the parent App so the new message bubbles render. */
  onWorkspacesFileUpdate: (f: WorkspacesFile) => void;
}

export function LayoutView(p: Props) {
  return (
    <Show
      when={p.node.kind === "split"}
      fallback={<LeafPane all={p} pane={p.node as Extract<LayoutNode, { kind: "pane" }>} />}
    >
      <SplitView
        {...(p.node as Extract<LayoutNode, { kind: "split" }>)}
        all={p}
      />
    </Show>
  );
}

// Phase 8.A/regression-fix: render a leaf pane. Extracted from the previous
// inline IIFE (`fallback={(() => { ... })()}`) — the IIFE was re-evaluated on
// every parent render, which under some conditions caused the leaf component
// to thrash mount/unmount and lose click events on Connect / sidebar items.
// As a stable component, Solid reuses the same instance across re-renders.
function LeafPane(props: { all: Props; pane: Extract<LayoutNode, { kind: "pane" }> }) {
  const isActive = () => props.all.activePaneId === props.pane.pane_id;
  const kind = () => paneKindOf(props.pane);
  // Phase 16: SSH detection moved up to the workspace level. Reading
  // it off the file-manager pane's own connection (the previous code
  // path) always returned false because file-manager panes don't
  // carry one — that left the file manager rendering only the local
  // column even in clearly-SSH workspaces.
  const workspaceIsSsh = () => props.all.workspaceIsSsh;
  return (
    <Switch
      fallback={
        <PaneView
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          workspaceConnection={props.all.workspaceConnection}
          isActive={isActive()}
          isConnected={props.all.connectedPaneIds.has(props.pane.pane_id)}
          pendingPasswordFor={props.all.pendingPasswordFor}
          pendingPassphrase={props.all.pendingPassphrase}
          pendingHostTrust={props.all.pendingHostTrust}
          status={props.all.paneStatus[props.pane.pane_id]}
          statusText={props.all.paneStatusText[props.pane.pane_id]}
          ensureTerm={props.all.ensureTerm}
          onFocus={props.all.onFocus}
          onConnect={props.all.onConnect}
          onSplit={props.all.onSplit}
          onClose={props.all.onClose}
          onDisconnect={props.all.onDisconnect}
          tmuxSession={props.all.panePersistence[props.pane.pane_id] ?? null}
          onKillSession={props.all.onKillSession}
          onSetTitle={props.all.onSetTitle}
          onSetAnnotation={props.all.onSetAnnotation}
        />
      }
    >
      <Match when={kind() === "browser"}>
        <BrowserPane
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          isActive={isActive()}
          onFocus={props.all.onFocus}
          onSplit={props.all.onSplit}
          onClose={props.all.onClose}
          onNavigate={props.all.onBrowserNavigate}
          onGoBack={props.all.onBrowserGoBack}
          onGoHome={props.all.onBrowserGoHome}
          onSetForward={props.all.onBrowserSetForward}
          onSetTitle={props.all.onSetTitle}
          onSetAnnotation={props.all.onSetAnnotation}
        />
      </Match>
      <Match when={kind() === "filemanager"}>
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn">🗂 file manager</span>
            <button
              class="pane-btn pane-close"
              title="Close"
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              ×
            </button>
          </div>
          <div class="pane-body">
            <FileManagerPane
              workspaceId={props.all.workspaceId}
              hasSsh={workspaceIsSsh()}
              hasActiveSession={props.all.connectedPaneIds.size > 0}
            />
          </div>
        </div>
      </Match>
      <Match when={kind() === "claudechat"}>
        <ClaudeChatPane
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          isActive={isActive()}
          onFocus={props.all.onFocus}
          onClose={props.all.onClose}
          onSetTitle={props.all.onSetTitle}
          onSetAnnotation={props.all.onSetAnnotation}
          onFileUpdate={props.all.onWorkspacesFileUpdate}
        />
      </Match>
    </Switch>
  );
}

// Keep Show imported so older usages elsewhere keep working.
void Show;

function SplitView(
  s: Extract<LayoutNode, { kind: "split" }> & { all: Props }
) {
  let containerRef!: HTMLDivElement;
  return (
    <div ref={containerRef!} class={`split split-${s.direction}`}>
      <div class="split-side" style={{ flex: `${s.ratio}` }}>
        <LayoutView {...s.all} node={s.first} />
      </div>
      <Divider
        direction={s.direction}
        parentEl={() => containerRef}
        onDrag={(r) => s.all.onRatioDrag(s.split_id, r)}
        onCommit={(r) => s.all.onRatioCommit(s.split_id, r)}
      />
      <div class="split-side" style={{ flex: `${1 - s.ratio}` }}>
        <LayoutView {...s.all} node={s.second} />
      </div>
    </div>
  );
}
