import { Show } from "solid-js";
import { Divider } from "./Divider";
import {
  PaneView,
  type ConnectOpts,
  type HostTrustPending,
  type PassphrasePending,
} from "./PaneView";
import type { LayoutNode, SplitDirection } from "./types";
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
  ensureTerm: (paneId: string) => TerminalInstance;
  onFocus: (paneId: string) => void;
  onConnect: (paneId: string, opts?: ConnectOpts) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  onDisconnect: (paneId: string) => void;
  onRatioDrag: (splitId: string, ratio: number) => void;
  onRatioCommit: (splitId: string, ratio: number) => void;
}

export function LayoutView(p: Props) {
  return (
    <Show
      when={p.node.kind === "split"}
      fallback={
        <PaneView
          workspaceId={p.workspaceId}
          pane={p.node as Extract<LayoutNode, { kind: "pane" }>}
          isActive={
            p.activePaneId ===
            (p.node as Extract<LayoutNode, { kind: "pane" }>).pane_id
          }
          isConnected={p.connectedPaneIds.has(
            (p.node as Extract<LayoutNode, { kind: "pane" }>).pane_id
          )}
          pendingPasswordFor={p.pendingPasswordFor}
          pendingPassphrase={p.pendingPassphrase}
          pendingHostTrust={p.pendingHostTrust}
          status={
            p.paneStatus[
              (p.node as Extract<LayoutNode, { kind: "pane" }>).pane_id
            ]
          }
          ensureTerm={p.ensureTerm}
          onFocus={p.onFocus}
          onConnect={p.onConnect}
          onSplit={p.onSplit}
          onClose={p.onClose}
          onDisconnect={p.onDisconnect}
        />
      }
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
