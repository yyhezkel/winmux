import type { SplitDirection } from "./types";

interface Props {
  direction: SplitDirection;
  parentEl: () => HTMLElement | null;
  onDrag: (ratio: number) => void;
  onCommit: (ratio: number) => void;
}

export function Divider(p: Props) {
  let frame = 0;
  let pending = -1;

  const onMouseDown = (e: MouseEvent) => {
    e.preventDefault();
    const parent = p.parentEl();
    if (!parent) return;
    const rect = parent.getBoundingClientRect();

    // Phase RTL-fix: in horizontal splits, flexbox row direction
    // flips under `dir="rtl"` so the "first" pane visually sits on
    // the RIGHT. (clientX - rect.left) / rect.width measures from
    // the physical left edge, which corresponds to the SECOND pane
    // in RTL — meaning a raw computation makes dragging right
    // shrink whatever the user thinks they're growing. Flip the
    // ratio in RTL so it always tracks the first (start-side) pane.
    const isRtl = document.documentElement.dir === "rtl";
    const computeRatio = (ev: MouseEvent): number => {
      if (p.direction === "horizontal") {
        const raw = (ev.clientX - rect.left) / rect.width;
        const r = isRtl ? 1 - raw : raw;
        return Math.min(0.95, Math.max(0.05, r));
      }
      // Vertical splits stack top-to-bottom regardless of writing
      // direction — no RTL flip needed.
      return Math.min(0.95, Math.max(0.05, (ev.clientY - rect.top) / rect.height));
    };

    const onMove = (ev: MouseEvent) => {
      const r = computeRatio(ev);
      pending = r;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        if (pending >= 0) p.onDrag(pending);
      });
    };
    const onUp = (ev: MouseEvent) => {
      if (frame) cancelAnimationFrame(frame);
      frame = 0;
      const r = computeRatio(ev);
      p.onCommit(r);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
    };

    document.body.style.userSelect = "none";
    document.body.style.cursor =
      p.direction === "horizontal" ? "col-resize" : "row-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div
      class={`divider divider-${p.direction}`}
      onMouseDown={onMouseDown}
    />
  );
}
