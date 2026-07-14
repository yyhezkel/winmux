// beta.3 (pane-dragdrop): shared pointer-drag store for pane reorder.
//
// Rationale: pane-drag state has to be visible to BOTH the pane being
// dragged (source shows .pane-dragging) AND every other pane in the
// workspace (a hovered target shows .pane-drop-target). Rather than
// thread new props through LayoutView -> SplitView -> LeafPane ->
// PaneView we keep the state at module scope. This mirrors the
// sidebar's pattern (component-scope signals + window-level listeners)
// but hoisted to the module so multiple PaneView instances can read
// it directly.
//
// The App registers a swap handler on mount (setPaneSwapHandler). The
// gesture is driven entirely by pointer events (no HTML5 DnD) — same
// reasoning as beta.3-ws-dragdrop: Tauri's WebView2 OS drop handler
// stays enabled for Phase 49-A file drops onto terminals, and on
// Windows that handler swallows in-page HTML5 drags.
//
// MVP scope (Yossi's brief): only the CENTER drop swaps the two panes.
// Left/right/top/bottom half detection lives in `computeDropZone` and
// is exposed for CSS ("half-tint" highlight) but does NOT perform a
// split-creation — that's Phase 2. The drop-on-half path currently
// falls back to a swap so a user who drags to a half still gets a
// useful action.

import { createSignal } from "solid-js";

export type DropZone = "center" | "left" | "right" | "top" | "bottom";

export type PaneSwapHandler = (
  paneAId: string,
  paneBId: string,
) => void | Promise<void>;

const [dragPaneId, setDragPaneId] = createSignal<string | null>(null);
const [dragLabel, setDragLabel] = createSignal<string>("");
const [ghostPos, setGhostPos] = createSignal<{ x: number; y: number } | null>(null);
const [dropTargetId, setDropTargetId] = createSignal<string | null>(null);
const [dropZone, setDropZone] = createSignal<DropZone | null>(null);

export const paneDragStore = {
  dragPaneId,
  dragLabel,
  ghostPos,
  dropTargetId,
  dropZone,
};

let swapHandler: PaneSwapHandler | null = null;
export function setPaneSwapHandler(fn: PaneSwapHandler | null): void {
  swapHandler = fn;
}

// Non-reactive scratch state — mirrors the sidebar's `pending` /
// `didDrag` pair. `pending` holds the press until the move threshold
// is crossed; `didDrag` swallows the trailing click after a completed
// drag so a pane-header drag doesn't also fire onFocus twice.
const DRAG_THRESHOLD = 5;
type Pending = {
  paneId: string;
  label: string;
  startX: number;
  startY: number;
};
let pending: Pending | null = null;
let didDrag = false;

export function paneDragDidDrag(): boolean {
  return didDrag;
}

// Compute which half (or center) of a pane the cursor is over. The
// center band is ~20% wide so a user can reliably hit "swap" without
// drifting into a half. RTL is unaffected — left/right are still
// screen-left/right (the CSS mirrors the highlight via inset-inline).
export function computeDropZone(
  el: HTMLElement,
  x: number,
  y: number,
): DropZone {
  const r = el.getBoundingClientRect();
  const cx = (x - r.left) / r.width; // 0..1
  const cy = (y - r.top) / r.height;
  const CENTER = 0.2; // radius from midpoint that counts as center
  if (Math.abs(cx - 0.5) <= CENTER && Math.abs(cy - 0.5) <= CENTER) {
    return "center";
  }
  // Pick the dominant axis. Whichever half the cursor is deepest in
  // wins, so a diagonal cursor picks the closest edge.
  const dx = Math.abs(cx - 0.5);
  const dy = Math.abs(cy - 0.5);
  if (dx >= dy) {
    return cx < 0.5 ? "left" : "right";
  }
  return cy < 0.5 ? "top" : "bottom";
}

function updateDropTarget(x: number, y: number): void {
  const src = dragPaneId();
  if (!src) {
    setDropTargetId(null);
    setDropZone(null);
    return;
  }
  const under = document.elementFromPoint(x, y) as HTMLElement | null;
  if (!under) {
    setDropTargetId(null);
    setDropZone(null);
    return;
  }
  const paneEl = under.closest<HTMLElement>("[data-pane-id]");
  if (!paneEl) {
    setDropTargetId(null);
    setDropZone(null);
    return;
  }
  const targetId = paneEl.dataset.paneId ?? "";
  if (!targetId || targetId === src) {
    setDropTargetId(null);
    setDropZone(null);
    return;
  }
  setDropTargetId(targetId);
  setDropZone(computeDropZone(paneEl, x, y));
}

function endGesture(): void {
  window.removeEventListener("pointermove", onWinPointerMove);
  window.removeEventListener("pointerup", onWinPointerUp);
  window.removeEventListener("pointercancel", onWinPointerCancel);
  document.body.classList.remove("winmux-dragging");
  setDragPaneId(null);
  setDragLabel("");
  setGhostPos(null);
  setDropTargetId(null);
  setDropZone(null);
  pending = null;
}

function abortDrag(): void {
  endGesture();
  didDrag = false;
}

function onWinPointerMove(e: PointerEvent): void {
  if (!pending) return;
  if (!didDrag) {
    if (
      Math.hypot(e.clientX - pending.startX, e.clientY - pending.startY) <
      DRAG_THRESHOLD
    ) {
      return;
    }
    didDrag = true;
    setDragPaneId(pending.paneId);
    setDragLabel(pending.label);
    document.body.classList.add("winmux-dragging");
  }
  setGhostPos({ x: e.clientX, y: e.clientY });
  updateDropTarget(e.clientX, e.clientY);
}

function onWinPointerUp(): void {
  const wasDrag = didDrag;
  const src = dragPaneId();
  const dst = dropTargetId();
  // MVP: every drop zone (center + halves) triggers a swap. Split-
  // creation for the halves is Phase 2. Same-pane drop is filtered
  // out by updateDropTarget already.
  endGesture();
  if (wasDrag && src && dst && src !== dst && swapHandler) {
    void swapHandler(src, dst);
  }
  // didDrag stays true until the next pointerdown resets it — the
  // browser fires a click right after pointerup and PaneView's own
  // handlers check paneDragDidDrag() to swallow it.
}

function onWinPointerCancel(): void {
  abortDrag();
}

// Called from PaneView's .pane-conn onPointerDown. Left-button only;
// bails on interactive children so their own click handlers keep
// running (buttons, inputs, etc.).
export function startPaneDrag(
  paneId: string,
  label: string,
  e: PointerEvent,
): void {
  if (e.button !== 0) return;
  const el = e.target as HTMLElement | null;
  if (!el) return;
  if (el.closest("button, input, textarea, select, .pane-btn")) return;
  didDrag = false;
  pending = { paneId, label, startX: e.clientX, startY: e.clientY };
  window.addEventListener("pointermove", onWinPointerMove);
  window.addEventListener("pointerup", onWinPointerUp);
  window.addEventListener("pointercancel", onWinPointerCancel);
}

// Global Escape aborts an in-flight drag. Installed once from
// LayoutView's onMount so PaneView doesn't need to duplicate the
// keydown listener per pane.
export function installPaneDragEscape(): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && (pending || dragPaneId() !== null)) {
      abortDrag();
    }
  };
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
}
