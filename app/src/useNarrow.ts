import { createSignal, onCleanup } from "solid-js";

// Width-based "compact" detector. Attach the returned `ref` to a container;
// `narrow()` flips true whenever the container's content width drops below
// `threshold` px. Used to collapse toolbars/tab-rows to icon-only when there's
// no room for labels (the label then survives as the button's `title`).
//
// ResizeObserver is already used elsewhere for canvas/terminal sizing
// (terminalInstance.ts, BrowserPane.tsx); this is the same primitive applied to
// a class toggle.

export function createNarrow(threshold: number): {
  ref: (el: HTMLElement) => void;
  narrow: () => boolean;
} {
  const [narrow, setNarrow] = createSignal(false);
  let observer: ResizeObserver | undefined;

  const ref = (el: HTMLElement) => {
    observer?.disconnect();
    observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setNarrow(entry.contentRect.width < threshold);
      }
    });
    observer.observe(el);
  };

  onCleanup(() => observer?.disconnect());
  return { ref, narrow };
}
