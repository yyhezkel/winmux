import { createSignal, Show } from "solid-js";
import { t } from "./i18n";
import { AddonsTab } from "./AddonsTab";
import {
  clampToViewport,
  makeWindowControls,
  ResizeHandles,
  type Geometry,
} from "./floatingWindow";

// Phase 68 (UX): per-workspace Add-ons window. Add-ons live on the remote
// server, so they're managed per-workspace — opened from the workspace's
// right-click menu (and from the Insights monitor's "install" prompt). Wraps
// the shared AddonsTab in the floating-window chrome.
interface Props {
  open: boolean;
  workspaceId?: string;
  workspaceName?: string;
  onClose: () => void;
}

const DEFAULT_GEOMETRY: Geometry = { x: 220, y: 110, w: 640, h: 540 };
const MIN_W = 420;
const MIN_H = 320;

export function AddonsWindow(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(
    clampToViewport(DEFAULT_GEOMETRY, MIN_W, MIN_H),
  );
  const { onDragStart, onResizeStart } = makeWindowControls({
    geom,
    setGeom,
    minW: MIN_W,
    minH: MIN_H,
    closeGuardSelector: ".addons-win-x",
  });

  return (
    <Show when={p.open}>
      <div
        class="fm-window addons-window"
        style={{
          left: `${geom().x}px`,
          top: `${geom().y}px`,
          width: `${geom().w}px`,
          height: `${geom().h}px`,
        }}
      >
        <div class="fm-window-header" onMouseDown={onDragStart}>
          <span class="fm-window-title">
            🧩 {t("settings.addons.title")}
            {p.workspaceName ? ` — ${p.workspaceName}` : ""}
          </span>
          <button class="fm-window-x addons-win-x" onClick={p.onClose} title={t("common.close")}>
            ×
          </button>
        </div>
        <div class="fm-window-body" style="display:block; overflow:auto; padding:12px 14px;">
          <AddonsTab workspaceId={p.workspaceId} />
        </div>
        <ResizeHandles onStart={onResizeStart} />
      </div>
    </Show>
  );
}
