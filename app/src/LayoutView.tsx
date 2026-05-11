import { Match, Show, Switch } from "solid-js";
import { Divider } from "./Divider";
import { BrowserPane } from "./BrowserPane";
import { FileManagerPane } from "./FileManagerPane";
import {
  PaneView,
  type ConnectOpts,
  type HostTrustPending,
  type PassphrasePending,
} from "./PaneView";
import { paneKindOf, type LayoutNode, type SplitDirection } from "./types";
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
  // Phase 15.B: detect whether the workspace has any SSH-capable pane so
  // the file manager can decide whether to show the remote column. We
  // can't ask the backend cheaply here, so fall back to looking at the
  // pane's own connection (when present) — good enough for the MVP
  // since a file-manager pane in a workspace generally co-exists with
  // a terminal pane that has the connection metadata.
  const workspaceIsSsh = () =>
    props.pane.connection?.type === "ssh";
  return (
    <Switch
      fallback={
        <PaneView
          workspaceId={props.all.workspaceId}
          pane={props.pane}
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
              hasSsh={workspaceIsSsh() || props.all.connectedPaneIds.size > 0}
            />
          </div>
        </div>
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
