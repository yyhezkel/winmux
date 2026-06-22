import { For, Show, createSignal } from "solid-js";
import { collectPanes, findPane, type Workspace, type ForwardRow } from "./types";
import { t } from "./i18n";
import { TechText } from "./TechText";
import type { SidebarMode } from "./settings";

function workspaceBadge(w: Workspace): { label: string; cls: string; title: string } {
  if (!w.layout) {
    if (w.connection?.type === "ssh") return { label: "S", cls: "ssh", title: "SSH" };
    return { label: "L", cls: "local", title: "Local" };
  }
  const panes = collectPanes(w.layout);
  if (panes.length > 1) return { label: `${panes.length}`, cls: "split", title: `${panes.length} panes` };
  const first = findPane(w.layout, panes[0]);
  if (first?.pane_kind === "browser") return { label: "B", cls: "browser", title: "Browser" };
  if (first?.pane_kind === "filemanager") return { label: "F", cls: "filemanager", title: "File manager" };
  if (first?.connection?.type === "ssh") return { label: "S", cls: "ssh", title: "SSH" };
  return { label: "L", cls: "local", title: "Local" };
}

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  connectedIds: Set<string>;
  // Phase 26: workspaces that contain at least one pane with a
  // pending blocking permission request. Renders a pulsing dot on
  // the workspace row so the user can spot waiting work across
  // workspaces.
  waitingWorkspaceIds: Set<string>;
  onActivate: (id: string) => void;
  onCreate: () => void;
  /** Phase 14.A — open the server provisioning wizard. */
  onProvision: () => void;
  /** Phase 65.C — open the "connect to existing server" wizard (new ws). */
  onConnectExisting: () => void;
  /** Phase 38 — open the settings modal from the sidebar gear. */
  onOpenSettings: () => void;
  /** Phase 39 — open the notes window from the sidebar. */
  onOpenNotes: () => void;
  onAction: (
    id: string,
    action: "rename" | "edit" | "delete" | "disconnect",
  ) => void;
  // Phase 36.A / 39: all forwards across workspaces, for the per-
  // workspace inline 🌐 badge. Clicking the badge opens the Ports
  // window scoped to that workspace.
  allForwards: ForwardRow[];
  onOpenPorts: (workspaceId: string) => void;
  // Phase 39.A: global Ports button (opens the window on the "All
  // workspaces" tab, no workspace context).
  onOpenPortsGlobal: () => void;
  // Phase 60: onOpenBrowser / onOpenFiles props removed — the
  // buttons moved to the workspace header (App.tsx, next to + diff).
  // Phase 62.B (item I) / 65.P: two-mode sidebar — full / icons.
  mode: SidebarMode;
  onSetMode: (mode: SidebarMode) => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);

  return (
    <div class={`sidebar ${p.mode}`}>
      {/* Phase 62.C: header stacks vertically — logo (+ wordmark in full
          mode) on top, the collapse arrow on its own line below. */}
      <div class="sidebar-header">
        <div class="sidebar-brand-row">
        <svg
          class="sidebar-logo"
          viewBox="0 0 1024 1024"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <defs>
            <linearGradient id="sb-bg" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stop-color="#1d2330" />
              <stop offset="100%" stop-color="#0e1116" />
            </linearGradient>
            <linearGradient id="sb-acc" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0%" stop-color="#7aa2f7" />
              <stop offset="100%" stop-color="#4ec9b0" />
            </linearGradient>
          </defs>
          <rect width="1024" height="1024" rx="200" fill="url(#sb-bg)" />
          <rect
            x="20"
            y="20"
            width="984"
            height="984"
            rx="184"
            fill="none"
            stroke="#21262d"
            stroke-width="4"
          />
          <polyline
            points="300,330 560,512 300,694"
            fill="none"
            stroke="url(#sb-acc)"
            stroke-width="86"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
          <rect x="600" y="640" width="190" height="56" rx="28" fill="url(#sb-acc)" />
          <circle cx="848" cy="176" r="20" fill="#5cd87f" />
        </svg>
        <span class="sidebar-brand">{t("sidebar.title")}</span>
        </div>
        {/* Phase 62.B (item I) / 65.P: header toggle flips full ↔ icons
            — the only two modes. Same as Ctrl+B. Phase 62.C: on its own
            line below the logo. */}
        <button
          class="sidebar-collapse-btn"
          onClick={() => p.onSetMode(p.mode === "full" ? "icons" : "full")}
          title={p.mode === "full" ? t("sidebar.collapse.tooltip") : t("sidebar.expand.tooltip")}
          aria-label={p.mode === "full" ? t("sidebar.collapse.tooltip") : t("sidebar.expand.tooltip")}
        >
          {p.mode === "full" ? "«" : "»"}
        </button>
      </div>
      <div class="sidebar-list">
        <For each={p.workspaces}>
          {(w) => (
            <div
              class={`ws-item ${p.activeId === w.id ? "active" : ""} ${
                p.waitingWorkspaceIds.has(w.id) ? "has-waiting" : ""
              }`}
              data-has-color={w.color ? "true" : "false"}
              style={w.color ? `--ws-color: ${w.color}` : undefined}
              title={p.mode === "icons" ? w.name : undefined}
              onClick={() => p.onActivate(w.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setMenuFor(menuFor() === w.id ? null : w.id);
              }}
            >
              <span
                class="ws-dot"
                style={{ background: w.color || "#6b7682" }}
              />
              <span class="ws-name">
  <Show when={w.emoji}>{w.emoji} </Show>
  <TechText text={w.name} />
</span>
              {/* Phase 49-B: 🌿 chip when this workspace is anchored
                  to a git worktree. Path goes in the tooltip. */}
              <Show when={w.git_worktree}>
                <span class="ws-worktree-chip" title={w.git_worktree!}>🌿</span>
              </Show>
              <WorkspaceBadge w={w} />
              {/* Phase 36.A: inline port-forward badge. Click opens the
                  browser (1 forward) or surfaces the workspace's Ports
                  panel by activating it (>1). */}
              {(() => {
                const fwds = p.allForwards.filter((f) => f.workspace_id === w.id);
                return (
                  <Show when={fwds.length > 0}>
                    <span
                      class="ws-port-badge"
                      title={t(
                        fwds.length === 1
                          ? "ports.workspaceBadge.tooltipOne"
                          : "ports.workspaceBadge.tooltipMany",
                        { count: fwds.length },
                      )}
                      onClick={(e) => {
                        e.stopPropagation();
                        p.onOpenPorts(w.id);
                      }}
                    >
                      🌐 {fwds.length}
                    </span>
                  </Show>
                );
              })()}
              <Show when={p.connectedIds.has(w.id)}>
                <span class="ws-live" title={t("sidebar.workspaceConnectedTitle")} />
              </Show>
              <Show when={menuFor() === w.id}>
                <div
                  class="ws-menu"
                  onClick={(e) => {
                    e.stopPropagation();
                    setMenuFor(null);
                  }}
                >
                  <button onClick={() => p.onAction(w.id, "rename")}>
                    {t("ws.context.rename")}
                  </button>
                  <button onClick={() => p.onAction(w.id, "edit")}>
                    {t("ws.context.edit")}
                  </button>
                  {/* Phase 65.Q removed "Add this machine…" — joining an
                      existing server now goes through the main wizard. */}
                  <Show when={p.connectedIds.has(w.id)}>
                    <button onClick={() => p.onAction(w.id, "disconnect")}>
                      {t("ws.context.disconnect")}
                    </button>
                  </Show>
                  <button
                    class="danger"
                    onClick={() => p.onAction(w.id, "delete")}
                  >
                    {t("ws.context.delete")}
                  </button>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>
      {/* Phase 39: Notes + Settings + Ports row, then New workspace,
          then Provision server.
          Phase 62.B (item I): each button is an emoji icon + a text
          label span. In icons mode CSS hides the labels and stacks the
          row vertically, so these stay reachable as icons (with
          tooltips) instead of disappearing. */}
      <div class="sidebar-actions-row">
        <button class="ws-action-half" onClick={p.onOpenNotes} title={t("sidebar.notes.tooltip")}>
          <span class="ws-action-emoji">📝</span>
          <span class="ws-action-label">{t("sidebar.notes.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenSettings} title={t("sidebar.settings.tooltip")}>
          <span class="ws-action-emoji">⚙</span>
          <span class="ws-action-label">{t("sidebar.settings.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenPortsGlobal} title={t("sidebar.ports.tooltip")}>
          <span class="ws-action-emoji">🌐</span>
          <span class="ws-action-label">{t("sidebar.ports.label")}</span>
        </button>
      </div>
      {/* Phase 60 (smoke-test 2a): the Browser + Files row moved to
          the workspace header next to "+ diff" — they're workspace-
          scoped tools and Yossi found them misplaced here. */}
      <button class="ws-add" onClick={p.onCreate} title={t("sidebar.new_workspace")}>
        <span class="ws-action-emoji">＋</span>
        <span class="ws-action-label">{t("sidebar.new_workspace")}</span>
      </button>
      <button class="ws-provision" onClick={p.onProvision} title={t("sidebar.provision_server_tooltip")}>
        <span class="ws-action-emoji">☁</span>
        <span class="ws-action-label">{t("sidebar.provision_server")}</span>
      </button>
      {/* Phase 65.C: connect to an already-running server (this machine
          joins it with its own SSH key). */}
      <button class="ws-provision" onClick={p.onConnectExisting} title={t("sidebar.connect_existing_tooltip")}>
        <span class="ws-action-emoji">🔗</span>
        <span class="ws-action-label">{t("sidebar.connect_existing")}</span>
      </button>
    </div>
  );
}

// Regression-fix v2: extracted from an inline IIFE that was re-evaluated on every
// parent render. The IIFE form caused churn that intermittently mis-routed clicks
// on the workspace items themselves and (separately) drove a `workspace_set_active`
// autosave loop. As a stable child component, Solid reuses the same instance.
function WorkspaceBadge(props: { w: Workspace }) {
  const b = () => workspaceBadge(props.w);
  return (
    <span class={`ws-badge ${b().cls}`} title={b().title}>
      {b().label}
    </span>
  );
}
