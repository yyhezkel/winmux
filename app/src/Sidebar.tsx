import { For, Show, createSignal, createMemo } from "solid-js";
import { collectPanes, findPane, type Workspace, type WorkspaceGroup, type ForwardRow } from "./types";
import { t } from "./i18n";
import { TechText } from "./TechText";
import type { SidebarMode } from "./settings";

// cmux-A A2: eight-color palette for workspace group swatches. Kept
// intentionally small so a group's dot in the sidebar is easy to
// recognize at a glance. Values are theme-neutral hexes that read on
// both dark and light backgrounds.
export const GROUP_COLORS = [
  "#e0af68", // amber
  "#7aa2f7", // blue
  "#9ece6a", // green
  "#bb9af7", // purple
  "#f7768e", // pink
  "#f7768e", // red (shares hex with pink — kept for palette label parity)
  "#7dcfff", // cyan
  "#a0a8b2", // gray
];
// Re-export a de-duplicated list for the picker (the two red/pink slots
// use the same accent, so the picker shows seven visible tiles).
const GROUP_PICKER_COLORS = [
  "#e0af68",
  "#7aa2f7",
  "#9ece6a",
  "#bb9af7",
  "#f7768e",
  "#e06c75",
  "#7dcfff",
  "#a0a8b2",
];

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
  /** Phase 14.A — open the server provisioning wizard (Phase 65.R: its
   *  "existing" mode now hosts the connect-to-existing-server flow). */
  onProvision: () => void;
  /** Phase 38 — open the settings modal from the sidebar gear. */
  onOpenSettings: () => void;
  /** Phase 39 — open the notes window from the sidebar. */
  onOpenNotes: () => void;
  onAction: (
    id: string,
    action: "rename" | "edit" | "delete" | "disconnect" | "addons",
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
  // cmux-A A1: aggregate count of panes with pending OSC 9/99/777
  // activity notifications. Rendered as a small amber badge in the
  // sidebar header (both full + icons mode). Hidden when 0.
  pendingNotifCount: number;
  // cmux-A A2: workspace groups (collapsible sidebar sections). The
  // parent App owns the list — Sidebar renders it + delegates the
  // create/rename/color/delete/collapse actions back up.
  groups: WorkspaceGroup[];
  onGroupCreate: (name: string, color: string) => void;
  onGroupRename: (id: string, name: string) => void;
  onGroupSetColor: (id: string, color: string) => void;
  onGroupToggleCollapse: (id: string, isCollapsed: boolean) => void;
  onGroupDelete: (id: string) => void;
  onWorkspaceSetGroup: (workspaceId: string, groupId: string | null) => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);
  // Phase 65 (bug 4.4): the context menu must escape the sidebar's
  // scroll container. `.sidebar-list` has `overflow-y:auto`, which CSS
  // coerces `overflow-x` to non-visible too, so an absolutely-positioned
  // menu gets clipped at the (narrow, in icons mode) sidebar edge. We
  // render it `position:fixed` at the cursor instead, anchored here.
  const [menuPos, setMenuPos] = createSignal<{ x: number; y: number }>({
    x: 0,
    y: 0,
  });
  // cmux-A A2: local UI state for group interactions. `groupMenuFor` +
  // `groupMenuPos` mirror the workspace-menu pattern above so the group
  // context menu also escapes the .sidebar-list overflow clip.
  const [groupMenuFor, setGroupMenuFor] = createSignal<string | null>(null);
  const [groupMenuPos, setGroupMenuPos] = createSignal<{ x: number; y: number }>({
    x: 0,
    y: 0,
  });
  // "Move to group…" submenu on a workspace right-click. Null = closed;
  // set to the workspace_id whose menu is expanded.
  const [moveMenuFor, setMoveMenuFor] = createSignal<string | null>(null);
  // Inline "+ Group" flow: null = idle; string = the text the user is
  // typing before hitting Enter (or Esc to cancel).
  const [newGroupName, setNewGroupName] = createSignal<string | null>(null);
  // Rename inline flow, keyed by group_id (only one at a time).
  const [renamingGroup, setRenamingGroup] = createSignal<{
    id: string;
    name: string;
  } | null>(null);
  // Color picker popover keyed by group_id.
  const [colorPickerFor, setColorPickerFor] = createSignal<string | null>(null);

  // Bucket the workspaces by group. Ungrouped (group_id == null OR the
  // referenced group_id was deleted) renders in a leading "Ungrouped"
  // section — always shown, always uncollapsible, so the empty case
  // still has a home.
  const groupedWorkspaces = createMemo(() => {
    const validGroupIds = new Set(p.groups.map((g) => g.id));
    const ungrouped: Workspace[] = [];
    const byGroup = new Map<string, Workspace[]>();
    for (const w of p.workspaces) {
      const gid = w.group_id;
      if (gid && validGroupIds.has(gid)) {
        const list = byGroup.get(gid) ?? [];
        list.push(w);
        byGroup.set(gid, list);
      } else {
        ungrouped.push(w);
      }
    }
    return { ungrouped, byGroup };
  });

  const openMenuAt = (
    e: MouseEvent,
    setter: (v: { x: number; y: number }) => void,
  ) => {
    // Clamp so the fixed menu stays on-screen (≈160×190px).
    setter({
      x: Math.min(e.clientX, window.innerWidth - 200),
      y: Math.min(e.clientY, window.innerHeight - 260),
    });
  };

  const startNewGroup = () => {
    setNewGroupName("");
  };
  const commitNewGroup = () => {
    const name = (newGroupName() ?? "").trim();
    if (name.length > 0) {
      p.onGroupCreate(name, GROUP_PICKER_COLORS[0]);
    }
    setNewGroupName(null);
  };

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
        <Show when={p.pendingNotifCount > 0}>
          <span
            class="sidebar-notif-badge"
            title={t("notifications.pending_count", { count: p.pendingNotifCount })}
          >
            {p.pendingNotifCount > 99 ? "99+" : p.pendingNotifCount}
          </span>
        </Show>
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
        {/* Ungrouped section — always rendered, never collapsible. If
            the user has no groups at all, the whole sidebar is this one
            section and the "Ungrouped" header disappears (only shown
            once at least one group exists, to avoid noise). */}
        <Show when={p.groups.length > 0}>
          <div class="group-header" style="cursor: default">
            <span class="group-header-name">{t("sidebar.ungrouped")}</span>
            <span class="group-header-count">({groupedWorkspaces().ungrouped.length})</span>
          </div>
        </Show>
        <For each={groupedWorkspaces().ungrouped}>
          {(w) => renderWorkspaceItem(w)}
        </For>
        <For each={p.groups}>
          {(g) => {
            const members = () => groupedWorkspaces().byGroup.get(g.id) ?? [];
            const collapsed = () => g.is_collapsed;
            return (
              <>
                <div
                  class={`group-header ${collapsed() ? "group-collapsed" : ""}`}
                  onClick={() => p.onGroupToggleCollapse(g.id, !collapsed())}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    if (groupMenuFor() === g.id) {
                      setGroupMenuFor(null);
                      return;
                    }
                    openMenuAt(e, setGroupMenuPos);
                    setGroupMenuFor(g.id);
                    setColorPickerFor(null);
                  }}
                >
                  <span
                    class="group-swatch"
                    style={{ "--group-color": g.color || "#6b7682" } as any}
                  />
                  <Show
                    when={renamingGroup()?.id !== g.id}
                    fallback={
                      <input
                        class="group-inline-input"
                        value={renamingGroup()?.name ?? g.name}
                        autofocus
                        onClick={(e) => e.stopPropagation()}
                        onInput={(e) =>
                          setRenamingGroup({ id: g.id, name: e.currentTarget.value })
                        }
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            const nm = (renamingGroup()?.name ?? "").trim();
                            if (nm.length > 0) p.onGroupRename(g.id, nm);
                            setRenamingGroup(null);
                          } else if (e.key === "Escape") {
                            setRenamingGroup(null);
                          }
                        }}
                        onBlur={() => setRenamingGroup(null)}
                      />
                    }
                  >
                    <span class="group-header-name">
                      <TechText text={g.name} />
                    </span>
                  </Show>
                  <span class="group-header-count">({members().length})</span>
                  <span class="group-header-chevron">▼</span>
                </div>
                {/* Group context menu (fixed-position; keyed by g.id). */}
                <Show when={groupMenuFor() === g.id}>
                  <div
                    class="group-menu"
                    style={{
                      top: `${groupMenuPos().y}px`,
                      left: `${groupMenuPos().x}px`,
                    }}
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      onClick={() => {
                        setGroupMenuFor(null);
                        setRenamingGroup({ id: g.id, name: g.name });
                      }}
                    >
                      {t("sidebar.rename_group")}
                    </button>
                    <button
                      onClick={() => {
                        setGroupMenuFor(null);
                        setColorPickerFor(g.id);
                      }}
                    >
                      {t("sidebar.change_color")}
                    </button>
                    <button
                      class="danger"
                      onClick={() => {
                        setGroupMenuFor(null);
                        p.onGroupDelete(g.id);
                      }}
                    >
                      {t("sidebar.delete_group")}
                    </button>
                  </div>
                </Show>
                {/* Color picker popover — inline under the header. */}
                <Show when={colorPickerFor() === g.id}>
                  <div class="group-swatch-picker" onClick={(e) => e.stopPropagation()}>
                    <For each={GROUP_PICKER_COLORS}>
                      {(c) => (
                        <button
                          class={g.color === c ? "selected" : ""}
                          style={{ background: c }}
                          onClick={() => {
                            p.onGroupSetColor(g.id, c);
                            setColorPickerFor(null);
                          }}
                          aria-label={c}
                        />
                      )}
                    </For>
                  </div>
                </Show>
                <Show when={!collapsed()}>
                  <For each={members()}>{(w) => renderWorkspaceItem(w)}</For>
                </Show>
              </>
            );
          }}
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
      {/* cmux-A A2: create a new sidebar group. Inline text input,
          Enter commits, Esc cancels. Shown as a compact "+" button
          at the sidebar bottom, respecting icons-mode compact layout. */}
      <Show
        when={newGroupName() === null}
        fallback={
          <input
            class="group-inline-input"
            placeholder={t("sidebar.group_name_prompt")}
            value={newGroupName() ?? ""}
            autofocus
            onInput={(e) => setNewGroupName(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitNewGroup();
              else if (e.key === "Escape") setNewGroupName(null);
            }}
            onBlur={() => commitNewGroup()}
          />
        }
      >
        <button
          class="ws-add-group"
          onClick={startNewGroup}
          title={t("sidebar.new_group")}
        >
          {t("sidebar.new_group")}
        </button>
      </Show>
      {/* Phase 65.R: single entry — "Provision server" opens the wizard
          whose mode picker covers both "new server" and "connect to an
          existing server". The standalone 🔗 button was removed. */}
      <button class="ws-provision" onClick={p.onProvision} title={t("sidebar.provision_server_tooltip")}>
        <span class="ws-action-emoji">☁</span>
        <span class="ws-action-label">{t("sidebar.provision_server")}</span>
      </button>
    </div>
  );

  // Extracted helper so the same JSX renders both in the Ungrouped
  // section AND under each named group. Kept inside the component so
  // it can reach the local menu-state signals + the props closure.
  function renderWorkspaceItem(w: Workspace) {
    return (
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
          if (menuFor() === w.id) {
            setMenuFor(null);
            setMoveMenuFor(null);
            return;
          }
          openMenuAt(e, setMenuPos);
          setMenuFor(w.id);
          setMoveMenuFor(null);
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
            class="ws-menu ws-menu-fixed"
            style={{
              position: "fixed",
              top: `${menuPos().y}px`,
              left: `${menuPos().x}px`,
              // Neutralize the RTL `.ws-menu { right: 12px }` rule —
              // we anchor by left at the cursor in both directions.
              right: "auto",
              "z-index": "1000",
            }}
            onClick={(e) => {
              e.stopPropagation();
              // Don't auto-close when the user is drilling into the
              // Move-to-group submenu — that would swallow the click.
              if (moveMenuFor() !== w.id) {
                setMenuFor(null);
                setMoveMenuFor(null);
              }
            }}
          >
            <button onClick={() => p.onAction(w.id, "rename")}>
              {t("ws.context.rename")}
            </button>
            <button onClick={() => p.onAction(w.id, "edit")}>
              {t("ws.context.edit")}
            </button>
            {/* Phase 68 (UX): per-workspace add-ons (hooks, Insights,
                cli, tmux-conf) live on the remote — manage them here. */}
            <button onClick={() => p.onAction(w.id, "addons")}>
              {t("ws.context.addons")}
            </button>
            {/* cmux-A A2: Move-to-group submenu. Expands inline in the
                same menu; the outer menu doesn't close on click while
                the submenu is expanded (see the onClick guard above). */}
            <button
              onClick={() => {
                setMoveMenuFor(moveMenuFor() === w.id ? null : w.id);
              }}
            >
              {t("sidebar.move_to_group")}▸
            </button>
            <Show when={moveMenuFor() === w.id}>
              <div
                class="group-menu"
                style={{
                  position: "static",
                  "box-shadow": "none",
                  border: "0",
                  padding: "0",
                }}
              >
                <Show when={w.group_id}>
                  <button
                    onClick={() => {
                      p.onWorkspaceSetGroup(w.id, null);
                      setMenuFor(null);
                      setMoveMenuFor(null);
                    }}
                  >
                    {t("sidebar.move_out_of_group")}
                  </button>
                </Show>
                <For each={p.groups.filter((g) => g.id !== w.group_id)}>
                  {(g) => (
                    <button
                      onClick={() => {
                        p.onWorkspaceSetGroup(w.id, g.id);
                        setMenuFor(null);
                        setMoveMenuFor(null);
                      }}
                    >
                      <span
                        class="group-swatch"
                        style={{
                          "--group-color": g.color || "#6b7682",
                          "margin-inline-end": "6px",
                        } as any}
                      />
                      <TechText text={g.name} />
                    </button>
                  )}
                </For>
              </div>
            </Show>
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
    );
  }
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
