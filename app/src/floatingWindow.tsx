import { For, type Accessor, type Setter } from "solid-js";

// Phase 62 (item 2): shared drag + 8-way resize for the floating
// Browser / File-Manager windows. Both windows previously duplicated
// header-drag + a single bottom-right grip; this module unifies that
// and adds the four edges + four corners, with min-size clamping that
// keeps the opposite edge pinned when dragging a top/left handle.
//
// The geometry signal + its persistence stay owned by each window
// (different localStorage keys, different min sizes) — this module is
// pure mechanics over a passed-in signal.

export type Geometry = { x: number; y: number; w: number; h: number };

/** The 8 resize directions. `n`/`s`/`e`/`w` are edges; the rest corners. */
export type ResizeEdge = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

export const ALL_EDGES: ResizeEdge[] = [
  "n",
  "s",
  "e",
  "w",
  "ne",
  "nw",
  "se",
  "sw",
];

/** Phase 64 (N): clamp a window rect into the current viewport so it can
 *  never open larger than (or off the edge of) a small screen — the user
 *  must always be able to reach the header + resize grips. Width/height
 *  are capped to the viewport (but never below the min), then the origin
 *  is pulled in so the whole window stays on-screen. */
export function clampToViewport(g: Geometry, minW: number, minH: number): Geometry {
  const maxW = Math.max(minW, window.innerWidth - 16);
  const maxH = Math.max(minH, window.innerHeight - 16);
  const w = Math.min(Math.max(minW, g.w), maxW);
  const h = Math.min(Math.max(minH, g.h), maxH);
  const x = Math.max(0, Math.min(g.x, window.innerWidth - w - 8));
  const y = Math.max(0, Math.min(g.y, window.innerHeight - h - 8));
  return { x, y, w, h };
}

/** Load a persisted window rect from localStorage, always clamped into the
 *  current viewport (stored OR default) so it can't open off-screen. Shared
 *  by the unified panel floats; the older Browser/File windows keep their
 *  own inline copies. */
export function loadGeometry(key: string, def: Geometry, minW: number, minH: number): Geometry {
  try {
    const raw = localStorage.getItem(key);
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (
        parsed &&
        typeof parsed === "object" &&
        typeof (parsed as Geometry).x === "number" &&
        typeof (parsed as Geometry).y === "number" &&
        typeof (parsed as Geometry).w === "number" &&
        typeof (parsed as Geometry).h === "number"
      ) {
        return clampToViewport(parsed as Geometry, minW, minH);
      }
    }
  } catch {
    // Corrupt entry — fall through to default.
  }
  return clampToViewport(def, minW, minH);
}

export function saveGeometry(key: string, g: Geometry): void {
  try {
    localStorage.setItem(key, JSON.stringify(g));
  } catch {
    // Quota or private mode — ignore.
  }
}

/** Pure resize math — given the original rect, a pointer delta, and the
 *  edge being dragged, return the new rect. East/south grow with the
 *  delta directly; west/north move the origin AND shrink, clamped so the
 *  opposite edge stays put once the min size is hit. Exported for the
 *  (future) unit tests and reused by the live drag handler. */
export function resizeRect(
  edge: ResizeEdge,
  orig: Geometry,
  dx: number,
  dy: number,
  minW: number,
  minH: number,
): Geometry {
  let { x, y, w, h } = orig;
  if (edge.includes("e")) {
    w = Math.max(minW, orig.w + dx);
  }
  if (edge.includes("s")) {
    h = Math.max(minH, orig.h + dy);
  }
  if (edge.includes("w")) {
    // The right edge (orig.x + orig.w) is the anchor.
    const right = orig.x + orig.w;
    w = Math.max(minW, orig.w - dx);
    x = Math.max(0, right - w);
    // If clamping x at 0 shrank the available width, re-derive w so the
    // window never extends past the anchor.
    w = right - x >= minW ? right - x : w;
  }
  if (edge.includes("n")) {
    const bottom = orig.y + orig.h;
    h = Math.max(minH, orig.h - dy);
    y = Math.max(0, bottom - h);
    h = bottom - y >= minH ? bottom - y : h;
  }
  return { x, y, w, h };
}

/** Build the header-drag + per-edge resize mouse handlers bound to a
 *  geometry signal. Returns plain DOM event handlers; the window
 *  component wires `onDragStart` to its header and passes `onResizeStart`
 *  to <ResizeHandles>. `closeGuard` lets the header ignore drags that
 *  start on a control (e.g. the close button). */
export function makeWindowControls(opts: {
  geom: Accessor<Geometry>;
  setGeom: Setter<Geometry>;
  minW: number;
  minH: number;
  closeGuardSelector?: string;
}) {
  const { geom, setGeom, minW, minH } = opts;

  let dragState:
    | { startX: number; startY: number; origX: number; origY: number }
    | null = null;
  const onDragMove = (e: MouseEvent) => {
    if (!dragState) return;
    setGeom((g) => ({
      ...g,
      x: Math.max(0, dragState!.origX + (e.clientX - dragState!.startX)),
      y: Math.max(0, dragState!.origY + (e.clientY - dragState!.startY)),
    }));
  };
  const onDragUp = () => {
    dragState = null;
    window.removeEventListener("mousemove", onDragMove);
    window.removeEventListener("mouseup", onDragUp);
  };
  const onDragStart = (e: MouseEvent) => {
    if (
      opts.closeGuardSelector &&
      (e.target as HTMLElement).closest(opts.closeGuardSelector)
    ) {
      return;
    }
    // Phase 65 (bug 2.2): diagnostic — confirm the header mousedown
    // reaches us (visible in the debug build's devtools).
    console.log(
      "[winmux drag] onDragStart",
      (e.target as HTMLElement)?.className,
    );
    e.preventDefault();
    // Phase 65 (bug 2.2): stopPropagation to match onResizeStart (which
    // works). The only code difference between resize (works) and drag
    // (didn't) was this call — a bubble-phase ancestor mousedown handler
    // was apparently interfering with the header drag.
    e.stopPropagation();
    const g = geom();
    dragState = { startX: e.clientX, startY: e.clientY, origX: g.x, origY: g.y };
    window.addEventListener("mousemove", onDragMove);
    window.addEventListener("mouseup", onDragUp);
  };

  let resizeState: { startX: number; startY: number; orig: Geometry; edge: ResizeEdge } | null =
    null;
  const onResizeMove = (e: MouseEvent) => {
    if (!resizeState) return;
    const dx = e.clientX - resizeState.startX;
    const dy = e.clientY - resizeState.startY;
    setGeom(resizeRect(resizeState.edge, resizeState.orig, dx, dy, minW, minH));
  };
  const onResizeUp = () => {
    resizeState = null;
    window.removeEventListener("mousemove", onResizeMove);
    window.removeEventListener("mouseup", onResizeUp);
  };
  const onResizeStart = (edge: ResizeEdge) => (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    resizeState = { startX: e.clientX, startY: e.clientY, orig: geom(), edge };
    window.addEventListener("mousemove", onResizeMove);
    window.addEventListener("mouseup", onResizeUp);
  };

  return { onDragStart, onResizeStart };
}

/** Renders the 8 resize handles (4 edges + 4 corners) as absolutely
 *  positioned strips inside a `position: relative/fixed` window. Styling
 *  lives in App.css under `.fw-resize-*`. */
export function ResizeHandles(props: {
  onStart: (edge: ResizeEdge) => (e: MouseEvent) => void;
}) {
  return (
    <For each={ALL_EDGES}>
      {(edge) => (
        <div
          class={`fw-resize fw-resize-${edge}`}
          onMouseDown={props.onStart(edge)}
        />
      )}
    </For>
  );
}
