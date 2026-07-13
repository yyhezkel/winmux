import { For, Show, createSignal, createMemo, onCleanup, onMount } from "solid-js";
import { collectPanes, findPane, type Workspace, type WorkspaceGroup, type ForwardRow } from "./types";
import { t } from "./i18n";
import { TechText } from "./TechText";
import {
  IconNotes,
  IconSettings,
  IconGlobe,
  IconGitBranch,
  IconPlus,
  IconCloud,
  IconChevronDown,
} from "./icons";
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
  // beta.3 (ws-dragdrop): direct drag-reorder. `newIndex` is a 0-based
  // slot within the destination scope (workspaces) or the group list.
  // The App handler wraps these to call `workspace_reorder` /
  // `workspace_group_reorder` and reload the workspaces file.
  onWorkspaceReorder: (
    workspaceId: string,
    groupId: string | null,
    newIndex: number,
  ) => void;
  onGroupReorder: (groupId: string, newIndex: number) => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);
  // Phase 65 (bug 4.4): the context menu must escape the sidebar's
  // scroll container. `.sidebar-list` has `overflow-y:auto`, which CSS
  // coerces `overflow-x` to non-visible too, so an absolutely-positioned
  // menu gets clipped at the (narrow, in icons mode) sidebar edge. We
  // render it `position:fixed` at the cursor instead, anchored here.
  const [menuPos, setMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 });
  const [groupMenuFor, setGroupMenuFor] = createSignal<string | null>(null);
  const [groupMenuPos, setGroupMenuPos] = createSignal<{ x: number; y: number }>({ x: 0, y: 0 });
  const [moveMenuFor, setMoveMenuFor] = createSignal<string | null>(null);
  const [newGroupName, setNewGroupName] = createSignal<string | null>(null);
  const [renamingGroup, setRenamingGroup] = createSignal<{ id: string; name: string } | null>(null);
  const [colorPickerFor, setColorPickerFor] = createSignal<string | null>(null);

  // beta.3 (ws-dragdrop): within each bucket workspaces sort by
  // `sort_order` ascending (nulls → end, ties broken by insertion
  // order). A pre-beta.3 workspaces.json has no sort_order at all —
  // that path reduces to the previous insertion-order rendering.
  const groupedWorkspaces = createMemo(() => {
    const validGroupIds = new Set(p.groups.map((g) => g.id));
    const ungrouped: { w: Workspace; ins: number }[] = [];
    const byGroup = new Map<string, { w: Workspace; ins: number }[]>();
    p.workspaces.forEach((w, ins) => {
      const gid = w.group_id;
      if (gid && validGroupIds.has(gid)) {
        const list = byGroup.get(gid) ?? [];
        list.push({ w, ins });
        byGroup.set(gid, list);
      } else {
        ungrouped.push({ w, ins });
      }
    });
    const cmp = (a: { w: Workspace; ins: number }, b: { w: Workspace; ins: number }) => {
      const ao = a.w.sort_order ?? Number.MAX_SAFE_INTEGER;
      const bo = b.w.sort_order ?? Number.MAX_SAFE_INTEGER;
      if (ao !== bo) return ao - bo;
      return a.ins - b.ins;
    };
    ungrouped.sort(cmp);
    for (const list of byGroup.values()) list.sort(cmp);
    return {
      ungrouped: ungrouped.map((x) => x.w),
      byGroup: new Map(
        Array.from(byGroup.entries()).map(([k, v]) => [k, v.map((x) => x.w)]),
      ),
    };
  });

  // beta.3 (ws-dragdrop): render groups in sort_order (nulls → end).
  const sortedGroups = createMemo(() => {
    const arr = p.groups.map((g, ins) => ({ g, ins }));
    arr.sort((a, b) => {
      const ao = a.g.sort_order ?? Number.MAX_SAFE_INTEGER;
      const bo = b.g.sort_order ?? Number.MAX_SAFE_INTEGER;
      if (ao !== bo) return ao - bo;
      return a.ins - b.ins;
    });
    return arr.map((x) => x.g);
  });

  // beta.3 (ws-dragdrop): drag state.
  type Drop =
    | { kind: "ws-line"; targetId: string; where: "above" | "below" }
    | { kind: "group-line"; targetId: string; where: "above" | "below" }
    | { kind: "into-group"; targetId: string | null }
    | null;
  const [dragKind, setDragKind] = createSignal<"ws" | "group" | null>(null);
  const [dragId, setDragId] = createSignal<string | null>(null);
  const [drop, setDrop] = createSignal<Drop>(null);
  const cancelDrag = () => {
    setDragKind(null);
    setDragId(null);
    setDrop(null);
  };
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && dragKind() !== null) cancelDrag();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });
  const whereByMidpoint = (ev: DragEvent, el: HTMLElement): "above" | "below" => {
    const r = el.getBoundingClientRect();
    return ev.clientY < r.top + r.height / 2 ? "above" : "below";
  };
  const scopeIndexOf = (scope: string | null, wsId: string): number => {
    const list = scope === null
      ? groupedWorkspaces().ungrouped
      : groupedWorkspaces().byGroup.get(scope) ?? [];
    return list.findIndex((w) => w.id === wsId);
  };
  const scopeSize = (scope: string | null): number => {
    const list = scope === null
      ? groupedWorkspaces().ungrouped
      : groupedWorkspaces().byGroup.get(scope) ?? [];
    return list.length;
  };
  const groupIndexOf = (gid: string): number => sortedGroups().findIndex((g) => g.id === gid);

  const openMenuAt = (e: MouseEvent, setter: (v: { x: number; y: number }) => void) => {
    setter({
      x: Math.min(e.clientX, window.innerWidth - 200),
      y: Math.min(e.clientY, window.innerHeight - 260),
    });
  };

  const startNewGroup = () => { setNewGroupName(""); };
  const commitNewGroup = () => {
    const name = (newGroupName() ?? "").trim();
    if (name.length > 0) {
      p.onGroupCreate(name, GROUP_PICKER_COLORS[0]);
    }
    setNewGroupName(null);
  };

  return (
    <div class={`sidebar ${p.mode}`}>
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
        <Show when={p.groups.length > 0}>
          <div
            class={`group-header ${
              drop()?.kind === "into-group" && (drop() as any).targetId === null
                ? "drop-into"
                : ""
            }`}
            style="cursor: default"
            onDragOver={(e) => {
              if (dragKind() !== "ws") return;
              e.preventDefault();
              if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
              setDrop({ kind: "into-group", targetId: null });
            }}
            onDrop={(e) => {
              if (dragKind() !== "ws" || !dragId()) return;
              e.preventDefault();
              const wsId = dragId()!;
              p.onWorkspaceReorder(wsId, null, scopeSize(null));
              cancelDrag();
            }}
          >
            <span class="group-header-name">{t("sidebar.ungrouped")}</span>
            <span class="group-header-count">({groupedWorkspaces().ungrouped.length})</span>
          </div>
        </Show>
        <For each={groupedWorkspaces().ungrouped}>
          {(w) => renderWorkspaceItem(w)}
        </For>
        <For each={sortedGroups()}>
          {(g) => {
            const members = () => groupedWorkspaces().byGroup.get(g.id) ?? [];
            const collapsed = () => g.is_collapsed;
            return (
              <>
                <div
                  class={`group-header ${collapsed() ? "group-collapsed" : ""} ${
                    dragKind() === "group" && dragId() === g.id ? "dragging" : ""
                  } ${
                    drop()?.kind === "into-group" && (drop() as any).targetId === g.id
                      ? "drop-into"
                      : ""
                  } ${
                    drop()?.kind === "group-line" && (drop() as any).targetId === g.id
                      ? `drop-${(drop() as any).where}`
                      : ""
                  }`}
                  draggable={true}
                  onDragStart={(e) => {
                    setDragKind("group");
                    setDragId(g.id);
                    setDrop(null);
                    if (e.dataTransfer) {
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.setData(
                        "application/x-winmux-drag",
                        JSON.stringify({ kind: "group", id: g.id }),
                      );
                    }
                  }}
                  onDragEnd={cancelDrag}
                  onDragOver={(e) => {
                    if (dragKind() === "ws") {
                      e.preventDefault();
                      if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
                      setDrop({ kind: "into-group", targetId: g.id });
                    } else if (dragKind() === "group" && dragId() !== g.id) {
                      e.preventDefault();
                      if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
                      const where = whereByMidpoint(e, e.currentTarget as HTMLElement);
                      setDrop({ kind: "group-line", targetId: g.id, where });
                    }
                  }}
                  onDrop={(e) => {
                    if (dragKind() === "ws" && dragId()) {
                      e.preventDefault();
                      const wsId = dragId()!;
                      const size = scopeSize(g.id);
                      p.onWorkspaceReorder(wsId, g.id, size);
                      cancelDrag();
                    } else if (dragKind() === "group" && dragId() && dragId() !== g.id) {
                      e.preventDefault();
                      const gid = dragId()!;
                      const targetIdx = groupIndexOf(g.id);
                      const draggingIdx = groupIndexOf(gid);
                      const w = whereByMidpoint(e, e.currentTarget as HTMLElement);
                      let idx = w === "above" ? targetIdx : targetIdx + 1;
                      if (draggingIdx !== -1 && draggingIdx < targetIdx) idx -= 1;
                      p.onGroupReorder(gid, Math.max(0, idx));
                      cancelDrag();
                    }
                  }}
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
                  <span class="group-header-chevron"><IconChevronDown size={12} /></span>
                </div>
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
      <div class="sidebar-actions-row">
        <button class="ws-action-half" onClick={p.onOpenNotes} title={t("sidebar.notes.tooltip")}>
          <span class="ws-action-emoji"><IconNotes /></span>
          <span class="ws-action-label">{t("sidebar.notes.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenSettings} title={t("sidebar.settings.tooltip")}>
          <span class="ws-action-emoji"><IconSettings /></span>
          <span class="ws-action-label">{t("sidebar.settings.tooltip")}</span>
        </button>
        <button class="ws-action-half" onClick={p.onOpenPortsGlobal} title={t("sidebar.ports.tooltip")}>
          <span class="ws-action-emoji"><IconGlobe /></span>
          <span class="ws-action-label">{t("sidebar.ports.label")}</span>
        </button>
      </div>
      <button class="ws-add" onClick={p.onCreate} title={t("sidebar.new_workspace")}>
        <span class="ws-action-emoji"><IconPlus /></span>
        <span class="ws-action-label">{t("sidebar.new_workspace")}</span>
      </button>
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
      <button class="ws-provision" onClick={p.onProvision} title={t("sidebar.provision_server_tooltip")}>
        <span class="ws-action-emoji"><IconCloud /></span>
        <span class="ws-action-label">{t("sidebar.provision_server")}</span>
      </button>
    </div>
  );

  function renderWorkspaceItem(w: Workspace) {
    return (
      <div
        class={`ws-item ${p.activeId === w.id ? "active" : ""} ${
          p.waitingWorkspaceIds.has(w.id) ? "has-waiting" : ""
        } ${dragKind() === "ws" && dragId() === w.id ? "dragging" : ""} ${
          drop()?.kind === "ws-line" && (drop() as any).targetId === w.id
            ? `drop-${(drop() as any).where}`
            : ""
        }`}
        data-has-color={w.color ? "true" : "false"}
        style={w.color ? `--ws-color: ${w.color}` : undefined}
        title={p.mode === "icons" ? w.name : undefined}
        // beta.3 (ws-dragdrop) regression fix: the drag refactor dropped the
        // click-to-switch handler. A plain click (no drag gesture) still fires
        // `click`; a completed HTML5 drag suppresses the trailing click, so
        // switching and reordering coexist without a flag.
        onClick={() => p.onActivate(w.id)}
        draggable={true}
        onDragStart={(e) => {
          setDragKind("ws");
          setDragId(w.id);
          setDrop(null);
          if (e.dataTransfer) {
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData(
              "application/x-winmux-drag",
              JSON.stringify({ kind: "ws", id: w.id }),
            );
          }
        }}
        onDragEnd={cancelDrag}
        onDragOver={(e) => {
          if (dragKind() !== "ws" || !dragId() || dragId() === w.id) return;
          e.preventDefault();
          if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
          const where = whereByMidpoint(e, e.currentTarget as HTMLElement);
          setDrop({ kind: "ws-line", targetId: w.id, where });
        }}
        onDrop={(e) => {
          if (dragKind() !== "ws" || !dragId() || dragId() === w.id) return;
          e.preventDefault();
          const draggedId = dragId()!;
          const destScope = w.group_id ?? null;
          const targetIdx = scopeIndexOf(destScope, w.id);
          const where = whereByMidpoint(e, e.currentTarget as HTMLElement);
          const srcScope = p.workspaces.find((ww) => ww.id === draggedId)?.group_id ?? null;
          const srcIdx = scopeIndexOf(srcScope, draggedId);
          let idx = where === "above" ? targetIdx : targetIdx + 1;
          if (srcScope === destScope && srcIdx !== -1 && srcIdx < targetIdx) {
            idx -= 1;
          }
          p.onWorkspaceReorder(draggedId, destScope, Math.max(0, idx));
          cancelDrag();
        }}
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
        <Show when={w.git_worktree}>
          <span class="ws-worktree-chip" title={w.git_worktree!}><IconGitBranch size={13} /></span>
        </Show>
        <WorkspaceBadge w={w} />
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
                <IconGlobe size={12} /> {fwds.length}
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
              right: "auto",
              "z-index": "1000",
            }}
            onClick={(e) => {
              e.stopPropagation();
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
            <button onClick={() => p.onAction(w.id, "addons")}>
              {t("ws.context.addons")}
            </button>
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
