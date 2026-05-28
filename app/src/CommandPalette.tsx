import { createMemo, createSignal, createEffect, For, Show, onCleanup } from "solid-js";
import { t } from "./i18n";

// Phase 35 (#1.3): VSCode-style command palette. Opened with
// Ctrl+Shift+P. The command list is supplied by the parent (App.tsx)
// so each command calls the SAME handler the existing UI already uses
// — the palette is a second entry point, not a reimplementation.

export interface Command {
  id: string;
  label: string;
  handler: () => void;
  // When present and returns false, the command is hidden (scope
  // constraint, e.g. pane commands with no active pane).
  enabled?: () => boolean;
}

interface Props {
  open: boolean;
  commands: Command[];
  onClose: () => void;
}

export function CommandPalette(p: Props) {
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal(0);
  let inputRef: HTMLInputElement | undefined;

  const visible = createMemo(() =>
    p.commands.filter((c) => (c.enabled ? c.enabled() : true)),
  );

  // Case-insensitive substring match on the label. v1 — no fuzzy lib.
  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    const list = visible();
    if (q.length === 0) return list;
    return list.filter((c) => c.label.toLowerCase().includes(q));
  });

  // Reset query + selection and focus the input each time it opens.
  createEffect(() => {
    if (p.open) {
      setQuery("");
      setSelected(0);
      queueMicrotask(() => inputRef?.focus());
    }
  });

  // Keep the selection in range as the filter narrows.
  createEffect(() => {
    const n = filtered().length;
    if (selected() >= n) setSelected(n === 0 ? 0 : n - 1);
  });

  const run = (cmd: Command | undefined) => {
    if (!cmd) return;
    p.onClose();
    // Defer so the modal is gone before the handler (which may open
    // another modal) fires.
    queueMicrotask(() => cmd.handler());
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      p.onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const n = filtered().length;
      if (n > 0) setSelected((i) => (i + 1) % n);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const n = filtered().length;
      if (n > 0) setSelected((i) => (i - 1 + n) % n);
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(filtered()[selected()]);
    }
  };

  // Global capture so Ctrl+Shift+P-opened palette grabs keys even
  // though the modal isn't a routed focus trap.
  createEffect(() => {
    if (!p.open) return;
    const handler = (e: KeyboardEvent) => onKeyDown(e);
    window.addEventListener("keydown", handler, true);
    onCleanup(() => window.removeEventListener("keydown", handler, true));
  });

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="command-palette" onClick={(e) => e.stopPropagation()}>
          <input
            ref={inputRef}
            class="command-palette-input"
            type="text"
            placeholder={t("cmd.palette.placeholder")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
          <div class="command-palette-list">
            <Show
              when={filtered().length > 0}
              fallback={<div class="command-palette-empty">{t("cmd.palette.empty")}</div>}
            >
              <For each={filtered()}>
                {(cmd, i) => (
                  <div
                    class={`command-palette-item ${i() === selected() ? "selected" : ""}`}
                    onMouseEnter={() => setSelected(i())}
                    onClick={() => run(cmd)}
                  >
                    {cmd.label}
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
