import { createMemo, createEffect, For, Show, onCleanup } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ForwardRow, Workspace } from "./types";
import { t } from "./i18n";

// Phase 40: floating Ports window, scoped to the CURRENTLY ACTIVE
// workspace only (the "All workspaces" tab was dropped). Opened from
// the sidebar 🌐 button or a workspace's 🌐 badge. Shows a prominent
// Active/Inactive auto-forward toggle and the list of live forwards.

interface Props {
  open: boolean;
  /** The currently active workspace — drives scope, name, toggle, color. */
  activeWorkspace: Workspace | null;
  forwards: ForwardRow[];
  onClose: () => void;
  onStop: (workspaceId: string, remotePort: number) => void;
  /** Flip auto_port_forward for a workspace (no-op for Local). */
  onToggleAutoForward: (workspaceId: string, enabled: boolean) => void;
}

export function PortsWindow(p: Props) {
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

  const ws = () => p.activeWorkspace;
  // Local workspaces have no SSH connection — auto-port-forward is meaningless.
  const isLocal = () => !!ws() && ws()!.connection == null;
  const enabled = () => (ws()?.auto_port_forward ?? false) && !isLocal();

  const rows = createMemo(() => {
    const id = ws()?.id;
    return id ? p.forwards.filter((f) => f.workspace_id === id) : [];
  });

  const openLocal = (localPort: number) => {
    void openUrl(`http://localhost:${localPort}`).catch((e) =>
      console.warn("openUrl failed", e),
    );
  };

  const toggle = () => {
    const w = ws();
    if (!w || isLocal()) return;
    p.onToggleAutoForward(w.id, !enabled());
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal ports-window" onClick={(e) => e.stopPropagation()}>
          <div class="settings-head">
            <h3>
              <Show when={ws()} fallback={t("ports.panel.title")}>
                {t("ports.window.title", { workspace: ws()!.name })}
              </Show>
            </h3>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>
              ×
            </button>
          </div>

          <Show
            when={ws()}
            fallback={
              <div class="ports-window-body">
                <p class="ports-panel-empty">{t("ports.window.empty.noWorkspace")}</p>
              </div>
            }
          >
            <button
              type="button"
              class="ports-toggle-row"
              classList={{ on: enabled(), off: !enabled() }}
              disabled={isLocal()}
              onClick={toggle}
              style={enabled() && ws()!.color ? `--ws-color: ${ws()!.color}` : undefined}
            >
              <span class="ports-toggle-state">
                {enabled()
                  ? `✓ ${t("ports.window.toggle.active")}`
                  : t("ports.window.toggle.inactive")}
              </span>
              <span class="ports-toggle-label">{t("ports.window.toggle.label")}</span>
            </button>

            <div class="ports-window-body">
              <Show
                when={rows().length > 0}
                fallback={
                  <p class="ports-panel-empty">
                    {enabled()
                      ? t("ports.window.empty.activeNoForwards")
                      : t("ports.window.empty.toggleOff")}
                  </p>
                }
              >
                <For each={rows()}>
                  {(f) => (
                    <div class="ports-row" title={`http://localhost:${f.local_port}`}>
                      <span class="ports-row-icon">🌐</span>
                      <span class="ports-row-label" onClick={() => openLocal(f.local_port)}>
                        {t("ports.row.activeOn", { port: f.local_port })}
                        <span class="ports-row-sub">
                          {t("ports.row.fromRemote", { remote: f.remote_port })}
                        </span>
                      </span>
                      <span class="ports-row-actions">
                        <button onClick={() => p.onStop(f.workspace_id, f.remote_port)}>
                          {t("ports.menu.stopForward")}
                        </button>
                      </span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </Show>
        </div>
      </div>
    </Show>
  );
}
