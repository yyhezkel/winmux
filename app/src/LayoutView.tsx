import { Show } from "solid-js";
import { Divider } from "./Divider";
import { BrowserPane } from "./BrowserPane";
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
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
  onRatioDrag: (splitId: string, ratio: number) => void;
  onRatioCommit: (splitId: string, ratio: number) => void;
  // Phase 8.A: browser-pane callbacks.
  onBrowserNavigate: (paneId: string, url: string) => void;
  onBrowserGoBack: (paneId: string) => void;
  onBrowserGoHome: (paneId: string) => void;
}

export function LayoutView(p: Props) {
  return (
    <Show
      when={p.node.kind === "split"}
      fallback={(() => {
        const pane = p.node as Extract<LayoutNode, { kind: "pane" }>;
        const isActive = p.activePaneId === pane.pane_id;
        if (paneKindOf(pane) === "browser") {
          return (
            <BrowserPane
              pane={pane}
              isActive={isActive}
              onFocus={p.onFocus}
              onSplit={p.onSplit}
              onClose={p.onClose}
              onNavigate={p.onBrowserNavigate}
              onGoBack={p.onBrowserGoBack}
              onGoHome={p.onBrowserGoHome}
              onSetTitle={p.onSetTitle}
              onSetAnnotation={p.onSetAnnotation}
            />
          );
        }
        return (
          <PaneView
            workspaceId={p.workspaceId}
            pane={pane}
            isActive={isActive}
            isConnected={p.connectedPaneIds.has(pane.pane_id)}
            pendingPasswordFor={p.pendingPasswordFor}
            pendingPassphrase={p.pendingPassphrase}
            pendingHostTrust={p.pendingHostTrust}
            status={p.paneStatus[pane.pane_id]}
            statusText={p.paneStatusText[pane.pane_id]}
            ensureTerm={p.ensureTerm}
            onFocus={p.onFocus}
            onConnect={p.onConnect}
            onSplit={p.onSplit}
            onClose={p.onClose}
            onDisconnect={p.onDisconnect}
            onSetTitle={p.onSetTitle}
            onSetAnnotation={p.onSetAnnotation}
          />
        );
      })()}
    >
      <SplitView
        {...(p.node as Extract<LayoutNode, { kind: "split" }>)}
        all={p}
      />
    </Show>
  );
}

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
