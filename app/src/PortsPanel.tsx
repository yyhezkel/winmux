import { createSignal, For, Show } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ForwardRow } from "./types";
import { t } from "./i18n";

// Phase 36 (#2.2): collapsible "Ports" panel for the active workspace.
// Lists the live auto-forwards; click a row to open localhost:<local>
// in the default browser, right-click for Copy URL / Stop forward.

interface Props {
  forwards: ForwardRow[];
  onStop: (remotePort: number) => void;
}

export function PortsPanel(p: Props) {
  const [collapsed, setCollapsed] = createSignal(false);
  const [menuFor, setMenuFor] = createSignal<number | null>(null);

  const urlFor = (f: ForwardRow) => `http://localhost:${f.local_port}`;

  const open = (f: ForwardRow) => {
    void openUrl(urlFor(f)).catch((e) => console.warn("openUrl failed", e));
  };
  const copy = async (f: ForwardRow) => {
    try {
      await navigator.clipboard.writeText(urlFor(f));
    } catch (e) {
      console.warn("clipboard write failed", e);
    }
    setMenuFor(null);
  };

  return (
    <div class="ports-panel">
      <div class="ports-panel-head" onClick={() => setCollapsed((v) => !v)}>
        <span class="ports-panel-caret">{collapsed() ? "▸" : "▾"}</span>
        <span class="ports-panel-title">{t("ports.panel.title")}</span>
        <Show when={p.forwards.length > 0}>
          <span class="ports-panel-count">{p.forwards.length}</span>
        </Show>
      </div>
      <Show when={!collapsed()}>
        <Show
          when={p.forwards.length > 0}
          fallback={<div class="ports-panel-empty">{t("ports.panel.empty")}</div>}
        >
          <For each={p.forwards}>
            {(f) => (
              <div
                class="ports-row"
                title={urlFor(f)}
                onClick={() => open(f)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenuFor(menuFor() === f.remote_port ? null : f.remote_port);
                }}
              >
                <span class="ports-row-icon">🌐</span>
                <span class="ports-row-label">
                  {f.remote_port} → localhost:{f.local_port}
                </span>
                <Show when={menuFor() === f.remote_port}>
                  <div class="ports-row-menu" onClick={(e) => e.stopPropagation()}>
                    <button
                      onClick={() => {
                        void copy(f);
                      }}
                    >
                      {t("ports.menu.copyUrl")}
                    </button>
                    <button
                      onClick={() => {
                        setMenuFor(null);
                        p.onStop(f.remote_port);
                      }}
                    >
                      {t("ports.menu.stopForward")}
                    </button>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </Show>
      </Show>
    </div>
  );
}
