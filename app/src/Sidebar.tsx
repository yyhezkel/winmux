import { For, Show, createSignal } from "solid-js";
import { collectPanes, findPane, type Workspace } from "./types";

function workspaceBadge(w: Workspace): { label: string; cls: string; title: string } {
  if (!w.layout) {
    if (w.connection?.type === "ssh") return { label: "S", cls: "ssh", title: "SSH" };
    return { label: "L", cls: "local", title: "Local" };
  }
  const panes = collectPanes(w.layout);
  if (panes.length > 1) return { label: `${panes.length}`, cls: "split", title: `${panes.length} panes` };
  const first = findPane(w.layout, panes[0]);
  if (first?.pane_kind === "browser") return { label: "B", cls: "browser", title: "Browser" };
  if (first?.connection?.type === "ssh") return { label: "S", cls: "ssh", title: "SSH" };
  return { label: "L", cls: "local", title: "Local" };
}

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  connectedIds: Set<string>;
  onActivate: (id: string) => void;
  onCreate: () => void;
  onAction: (id: string, action: "rename" | "edit" | "delete" | "disconnect") => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);

  return (
    <div class="sidebar">
      <div class="sidebar-header">
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
        <span class="sidebar-brand">winmux</span>
      </div>
      <div class="sidebar-list">
        <For each={p.workspaces}>
          {(w) => (
            <div
              class={`ws-item ${p.activeId === w.id ? "active" : ""}`}
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
              <span class="ws-name">{w.name}</span>
              <WorkspaceBadge w={w} />
              <Show when={p.connectedIds.has(w.id)}>
                <span class="ws-live" title="connected" />
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
                    Rename
                  </button>
                  <button onClick={() => p.onAction(w.id, "edit")}>
                    Edit…
                  </button>
                  <Show when={p.connectedIds.has(w.id)}>
                    <button onClick={() => p.onAction(w.id, "disconnect")}>
                      Disconnect
                    </button>
                  </Show>
                  <button
                    class="danger"
                    onClick={() => p.onAction(w.id, "delete")}
                  >
                    Delete
                  </button>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>
      <button class="ws-add" onClick={p.onCreate}>
        + New workspace
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
