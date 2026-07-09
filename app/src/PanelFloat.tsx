import { createEffect, createSignal, type JSX } from "solid-js";
import { PanelChrome } from "./PanelChrome";
import {
  loadGeometry,
  makeWindowControls,
  ResizeHandles,
  saveGeometry,
  type Geometry,
} from "./floatingWindow";

// Unified side-panel lifecycle: the shared floating-window surface. Owns
// geometry (persisted per storage key) + drag/8-way-resize (via
// floatingWindow) and renders a PanelChrome header, so every panel's
// "float" mode is one component instead of a per-panel re-implementation.
// The body is whatever mode-agnostic content the panel passes as children.

interface Props {
  /** localStorage key for this float's geometry (usually per panel + ws). */
  storageKey: string;
  defaultGeom: Geometry;
  minW: number;
  minH: number;
  icon?: JSX.Element;
  title: string;
  headerActions?: JSX.Element;
  bodyClass?: string;
  onCollapse: () => void; // ⇤ back to drawer
  onFullscreen: () => void; // ⛶
  onClose: () => void;
  children: JSX.Element;
}

export function PanelFloat(p: Props) {
  const [geom, setGeom] = createSignal<Geometry>(
    loadGeometry(p.storageKey, p.defaultGeom, p.minW, p.minH),
  );

  // Re-load when the storage key changes (e.g. active workspace switch) so
  // each scope keeps its own remembered rect.
  let lastKey = p.storageKey;
  createEffect(() => {
    if (p.storageKey !== lastKey) {
      lastKey = p.storageKey;
      setGeom(loadGeometry(p.storageKey, p.defaultGeom, p.minW, p.minH));
    }
  });

  createEffect(() => saveGeometry(p.storageKey, geom()));

  const { onDragStart, onResizeStart } = makeWindowControls({
    geom,
    setGeom,
    minW: p.minW,
    minH: p.minH,
    closeGuardSelector: ".panel-chrome-actions",
  });

  return (
    <div
      class="panel-float"
      style={{
        left: `${geom().x}px`,
        top: `${geom().y}px`,
        width: `${geom().w}px`,
        height: `${geom().h}px`,
      }}
    >
      <PanelChrome
        icon={p.icon}
        title={p.title}
        headerActions={p.headerActions}
        onHeaderMouseDown={onDragStart}
        onCollapse={p.onCollapse}
        onFullscreen={p.onFullscreen}
        onClose={p.onClose}
      />
      <div class={`panel-float-body ${p.bodyClass ?? ""}`}>{p.children}</div>
      <ResizeHandles onStart={onResizeStart} />
    </div>
  );
}
