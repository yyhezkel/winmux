import { createMemo, createSignal, For, Show } from "solid-js";
import { t } from "./i18n";
import {
  IconBot,
  IconBell,
  IconBan,
  IconHammer,
  IconCircle,
  IconCheck,
  IconTrash,
  type IconComponent,
} from "./icons";

// Unshipped-fivefer (#1): the Notification Center. A slide-in panel that
// unifies the two notification streams (OSC 9/99/777 from terminals +
// RPC/agent notifications from Claude hooks) into one filterable, read-aware
// timeline. State (the accumulating item list + read set) lives in App.tsx;
// this component is presentational + owns only the active filter.

export interface NotifItem {
  id: number;
  title: string;
  body: string;
  workspace_id: string | null;
  /** 66.G: originating pane (when known) so a click lands on the exact
   *  pane, not just the workspace. Optional — older backend
   *  `notification:new` payloads simply omit it. */
  pane_id?: string | null;
  timestamp_ms: number;
  kind: string; // agent | notification | error | build | mention
}

interface Props {
  items: NotifItem[];
  readIds: Set<number>;
  onClose: () => void;
  /** 66.G: workspaceId may be null when only the pane is known (OSC
   *  notifications) — the App side resolves the workspace from the pane. */
  onJump: (workspaceId: string | null, paneId?: string | null) => void;
  onMarkRead: (id: number) => void;
}

// Header controls (mark-all-read + clear) for whichever surface hosts the
// notification body — passed to SideDrawer/PanelChrome `headerActions`. The
// ✕ close button is owned by the surface, so it isn't here.
export function NotifHeaderActions(p: { onMarkAllRead: () => void; onClear: () => void }) {
  return (
    <>
      <button class="side-drawer-btn" title={t("notif.markAllRead")} onClick={p.onMarkAllRead}>
        <IconCheck />
      </button>
      <button class="side-drawer-btn" title={t("notif.clear")} onClick={p.onClear}>
        <IconTrash />
      </button>
    </>
  );
}

// Filters we can actually populate today. build/mention have no source yet
// (deferred) — no point showing empty tabs.
const FILTERS = ["all", "agent", "notification", "error"] as const;
type Filter = (typeof FILTERS)[number];

const KIND_ICON: Record<string, IconComponent> = {
  agent: IconBot,
  notification: IconBell,
  error: IconBan,
  build: IconHammer,
  mention: IconBell,
};
const iconFor = (k: string): IconComponent => KIND_ICON[k] ?? IconCircle;

function relTime(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return t("notif.time.now");
  const m = Math.round(s / 60);
  if (m < 60) return t("notif.time.min").replace("{n}", String(m));
  const h = Math.round(m / 60);
  if (h < 24) return t("notif.time.hour").replace("{n}", String(h));
  return t("notif.time.day").replace("{n}", String(Math.round(h / 24)));
}

export function NotificationCenter(p: Props) {
  const [filter, setFilter] = createSignal<Filter>("all");

  const filtered = createMemo(() => {
    const f = filter();
    const list = [...p.items].sort((a, b) => b.timestamp_ms - a.timestamp_ms);
    return f === "all" ? list : list.filter((n) => n.kind === f);
  });

  const click = (n: NotifItem) => {
    p.onMarkRead(n.id);
    if (n.workspace_id || n.pane_id) {
      p.onJump(n.workspace_id, n.pane_id ?? null);
      p.onClose();
    }
  };

  return (
    <>
      <div class="notif-filters">
        <For each={FILTERS}>
          {(f) => {
            const n = createMemo(() =>
              f === "all" ? p.items.length : p.items.filter((it) => it.kind === f).length,
            );
            return (
              <button
                class={`notif-filter ${filter() === f ? "active" : ""}`}
                onClick={() => setFilter(f)}
              >
                {t(`notif.filter.${f}`)}
                <Show when={n() > 0}>
                  <span class="notif-filter-count">{n()}</span>
                </Show>
              </button>
            );
          }}
        </For>
      </div>

      <div class="notif-list">
        <Show
          when={filtered().length > 0}
          fallback={
            <div class="notif-empty">
              <div class="notif-empty-icon" aria-hidden="true"><IconBell size={28} /></div>
              <div class="notif-empty-title">{t("notif.empty.title")}</div>
              <div class="notif-empty-desc">{t("notif.empty.desc")}</div>
            </div>
          }
        >
          <For each={filtered()}>
            {(n) => (
              <div
                class={`notif-item ${p.readIds.has(n.id) ? "read" : "unread"} ${n.workspace_id || n.pane_id ? "jumpable" : ""}`}
                onClick={() => click(n)}
              >
                <span class="notif-item-icon" aria-hidden="true">{iconFor(n.kind)({ size: 15 })}</span>
                <div class="notif-item-body">
                  <div class="notif-item-title">{n.title || n.body}</div>
                  <Show when={n.title && n.body}>
                    <div class="notif-item-summary">{n.body}</div>
                  </Show>
                  <div class="notif-item-meta">
                    <span class="notif-item-kind">{t(`notif.filter.${n.kind}`) || n.kind}</span>
                    <span class="notif-item-time">{relTime(n.timestamp_ms)}</span>
                    <Show when={n.workspace_id}>
                      <span class="notif-item-jump">↗ {t("notif.jump")}</span>
                    </Show>
                  </div>
                </div>
                <Show when={!p.readIds.has(n.id)}>
                  <span class="notif-item-dot" aria-hidden="true" />
                </Show>
              </div>
            )}
          </For>
        </Show>
      </div>
    </>
  );
}
