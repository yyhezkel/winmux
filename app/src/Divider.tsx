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

    const computeRatio = (ev: MouseEvent): number => {
      if (p.direction === "horizontal") {
        return Math.min(0.95, Math.max(0.05, (ev.clientX - rect.left) / rect.width));
      }
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
