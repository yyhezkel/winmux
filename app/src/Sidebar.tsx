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
  if (first?.connection.type === "ssh") return { label: "S", cls: "ssh", title: "SSH" };
  return { label: "L", cls: "local", title: "Local" };
}

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  connectedIds: Set<string>;
  onActivate: (id: string) => void;
  onCreate: () => void;
  onAction: (id: string, action: "rename" | "delete" | "disconnect") => void;
}

export function Sidebar(p: Props) {
  const [menuFor, setMenuFor] = createSignal<string | null>(null);

  return (
    <div class="sidebar">
      <div class="sidebar-header">winmux</div>
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
              {(() => {
                const b = workspaceBadge(w);
                return (
                  <span class={`ws-badge ${b.cls}`} title={b.title}>
                    {b.label}
                  </span>
                );
              })()}
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
