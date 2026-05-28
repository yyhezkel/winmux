import { createMemo, createSignal, createEffect, For, Show, onCleanup } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ForwardRow, Workspace } from "./types";
import { t } from "./i18n";

// Phase 39: floating Ports window (replaces the always-on sidebar
// panel). Opened by clicking a workspace's 🌐 badge. Two tabs:
// the scoped workspace, and "All workspaces".

interface Props {
  open: boolean;
  /** Workspace whose badge was clicked — the initial scoped tab. */
  workspaceId: string | null;
  forwards: ForwardRow[];
  workspaces: Workspace[];
  onClose: () => void;
  onStop: (workspaceId: string, remotePort: number) => void;
  /** Open workspace settings (to the auto-port-forward toggle). */
  onOpenSettings: (workspaceId: string) => void;
}

export function PortsWindow(p: Props) {
  const [tab, setTab] = createSignal<"workspace" | "all">("workspace");

  // Reset to the workspace tab each time it opens.
  createEffect(() => {
    if (p.open) setTab(p.workspaceId ? "workspace" : "all");
  });

  // Esc to close.
  createEffect(() => {
    if (!p.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        p.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const wsName = (id: string) =>
    p.workspaces.find((w) => w.id === id)?.name ?? id;

  const rows = createMemo(() => {
    if (tab() === "all") return p.forwards;
    const id = p.workspaceId;
    return id ? p.forwards.filter((f) => f.workspace_id === id) : [];
  });

  const open = (localPort: number) => {
    void openUrl(`http://localhost:${localPort}`).catch((e) => console.warn("openUrl failed", e));
  };
  const copy = async (localPort: number) => {
    try {
      await navigator.clipboard.writeText(`http://localhost:${localPort}`);
    } catch (e) {
      console.warn("clipboard write failed", e);
    }
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal ports-window" onClick={(e) => e.stopPropagation()}>
          <div class="settings-head">
            <h3>{t("ports.panel.title")}</h3>
            <div class="ports-window-head-actions">
              <Show when={p.workspaceId}>
                <button class="ports-window-settings" onClick={() => p.onOpenSettings(p.workspaceId!)}>
                  {t("ports.window.settings")}
                </button>
              </Show>
              <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
            </div>
          </div>

          <div class="ports-window-tabs">
            <button
              class={tab() === "workspace" ? "active" : ""}
              disabled={!p.workspaceId}
              onClick={() => setTab("workspace")}
            >
              {p.workspaceId ? wsName(p.workspaceId) : t("ports.window.thisWorkspace")}
            </button>
            <button class={tab() === "all" ? "active" : ""} onClick={() => setTab("all")}>
              {t("ports.window.allWorkspaces")}
            </button>
          </div>

          <div class="ports-window-body">
            <Show
              when={rows().length > 0}
              fallback={<p class="ports-panel-empty">{t("ports.window.empty")}</p>}
            >
              <For each={rows()}>
                {(f) => (
                  <div class="ports-row" title={`http://localhost:${f.local_port}`}>
                    <span class="ports-row-icon">🌐</span>
                    <span class="ports-row-label" onClick={() => open(f.local_port)}>
                      {t("ports.row.activeOn", { port: f.local_port })}
                      <span class="ports-row-sub">
                        {t("ports.row.fromRemote", { remote: f.remote_port })}
                        <Show when={tab() === "all"}> · {wsName(f.workspace_id)}</Show>
                      </span>
                    </span>
                    <span class="ports-row-actions">
                      <button onClick={() => void copy(f.local_port)}>{t("ports.menu.copyUrl")}</button>
                      <button onClick={() => p.onStop(f.workspace_id, f.remote_port)}>
                        {t("ports.menu.stopForward")}
                      </button>
                    </span>
                  </div>
                )}
              </For>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
