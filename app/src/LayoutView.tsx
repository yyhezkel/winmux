import { Match, Show, Switch } from "solid-js";
import { Divider } from "./Divider";
// Phase 53 (rebased): BrowserPane no longer imported — the Browser
// surface moved to the workspace-level floating BrowserWindow
// (sidebar 🌐). The file stays in the repo as historical reference
// in case any of its in-pane Webview wiring proves useful for a
// future iteration.
import { DiffPane } from "./DiffPane";
import { FileManagerPane } from "./FileManagerPane";
import { HelpPane } from "./HelpPane";
import { t } from "./i18n";
// Phase 24.D: ClaudeChatPane (Phase 22) + ClaudeLogPane (Phase 24.B)
// removed. Files deleted, Match arms below stripped.
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
} from "./types";
import type { TerminalInstance } from "./terminalInstance";

interface Props {
  workspaceId: string;
  node: LayoutNode;
  activePaneId: string | null;
  connectedPaneIds: Set<string>;
  // Phase 26: pane_ids with a pending blocking feed item — these
  // panes get the cmux-style notification ring.
  waitingPaneIds: Set<string>;
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
  // Phase 23.I: workspace name. PaneView's header falls back to it
  // when the pane has no user-set title.
  workspaceName?: string;
  // Phase 31: workspace identity. Threaded through to PaneView so each
  // pane can compute its effective identity (own override falls back
  // to the workspace's value).
  workspaceColor?: string;
  workspaceEmoji?: string;
  // Phase 24.D: onWorkspacesFileUpdate removed — its only consumers
  // were the (now-gone) ChatPane / ClaudeLogPane Match arms.
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
          workspaceName={props.all.workspaceName}
          workspaceColor={props.all.workspaceColor}
          workspaceEmoji={props.all.workspaceEmoji}
          isActive={isActive()}
          isWaiting={props.all.waitingPaneIds.has(props.pane.pane_id)}
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
        {/* Phase 53 (rebased): Browser is no longer a pane kind — it's
            a workspace-level floating window opened from the sidebar's
            Browser button. The 53.C load-time migration rewrites any
            existing Browser pane to Terminal on first boot, so this
            arm only ever renders if a user hand-edits workspaces.json
            after migration. Defensive placeholder + escape hatch. */}
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn">🌐 {t("browser.legacyPane.title")}</span>
            <button
              class="pane-btn pane-close"
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              ×
            </button>
          </div>
          <div class="pane-body legacy-pane-placeholder">
            <p>{t("browser.legacyPane.body")}</p>
          </div>
        </div>
      </Match>
      <Match when={kind() === "filemanager"}>
        <div
          class={`pane ${isActive() ? "active" : ""} ${
            props.all.waitingPaneIds.has(props.pane.pane_id) ? "waiting" : ""
          }`}
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
      <Match when={kind() === "diff"}>
        {/* Phase 50 (#2.4): live git diff pane. Self-contained — owns
            its own header (source dropdown + Refresh) and body. */}
        <DiffPane
          workspaceId={props.all.workspaceId}
          pane={props.pane}
          isActive={isActive()}
          onFocus={props.all.onFocus}
          onClose={props.all.onClose}
        />
      </Match>
      <Match when={kind() === "help"}>
        {/* Phase 33: in-app help pane. Self-contained — no SSH/PTY,
            no remote state. The header title comes from i18n keyed by
            the topic (e.g. "help.title.sshKeySetup"). */}
        <div
          class={`pane ${isActive() ? "active" : ""}`}
          onClick={() => props.all.onFocus(props.pane.pane_id)}
        >
          <div class="pane-header">
            <span class="pane-conn">
              {t(`help.title.${
                (props.pane.help_topic ?? "ssh-key-setup")
                  .split("-")
                  .map((s, i) => (i === 0 ? s : s.charAt(0).toUpperCase() + s.slice(1)))
                  .join("")
              }`)}
            </span>
            <button
              class="pane-btn pane-close"
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                props.all.onClose(props.pane.pane_id);
              }}
            >
              ×
            </button>
          </div>
          <div class="pane-body">
            <HelpPane topic={props.pane.help_topic ?? "ssh-key-setup"} />
          </div>
        </div>
      </Match>
      {/* Phase 24.D: ClaudeChat + ClaudeLog Match arms removed
          with their pane kinds. Legacy panes with those pane_kind
          values are aliased to Terminal at deserialize time, so
          the Switch fallback (PaneView) handles them. */}
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
