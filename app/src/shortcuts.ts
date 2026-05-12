// Phase 16: shortcut parsing + matching helper.
//
// User-typed shortcut strings live in `settings.shortcuts.<name>` as
// human-readable accelerators like "Ctrl+Shift+C" or "Ctrl+,". Same
// vocabulary in the JSON file (hand-editable) and the UI's "click to
// record" picker. The helper parses each accelerator once on settings
// load and exposes `matches(event, accelerator)` so dispatchers stay
// readable.
//
// Vocabulary:
//   - Modifiers: `Ctrl`, `Alt`, `Shift`, `Meta` (Windows / Cmd key)
//   - Keys: single characters (case-insensitive), digits, punctuation,
//     special names (`Enter`, `Escape`, `Tab`, `Space`, `F1`..`F12`,
//     `ArrowUp`/`Down`/`Left`/`Right`, `Backspace`, `Delete`, `Home`,
//     `End`, `PageUp`, `PageDown`, `Insert`)
//   - Joined by `+` with optional whitespace.

import { DEFAULT_SHORTCUTS, type ShortcutsSettings } from "./settings";

export interface ParsedShortcut {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /** The non-modifier key, in the normalized form `event.key` would
   *  produce (lower-case for letters, literal for punctuation, name
   *  for special keys). Empty string means "modifier-only" (invalid). */
  key: string;
}

/** Normalize a single token to the form we compare against `event.key`. */
function normalizeKey(token: string): string {
  const t = token.trim();
  if (t.length === 0) return "";
  // Single letters / digits — lowercase the letter form so we can
  // compare with `event.key.toLowerCase()` (event.key is uppercase
  // when Shift is held; we already track Shift separately).
  if (t.length === 1) return t.toLowerCase();
  // Named keys — preserve the canonical browser KeyboardEvent.key
  // capitalisation so the comparison hits.
  const lc = t.toLowerCase();
  const named: Record<string, string> = {
    enter: "Enter",
    escape: "Escape",
    esc: "Escape",
    tab: "Tab",
    space: " ",
    spacebar: " ",
    backspace: "Backspace",
    delete: "Delete",
    del: "Delete",
    home: "Home",
    end: "End",
    pageup: "PageUp",
    pagedown: "PageDown",
    insert: "Insert",
    up: "ArrowUp",
    down: "ArrowDown",
    left: "ArrowLeft",
    right: "ArrowRight",
    arrowup: "ArrowUp",
    arrowdown: "ArrowDown",
    arrowleft: "ArrowLeft",
    arrowright: "ArrowRight",
  };
  if (lc in named) return named[lc];
  // F1..F12
  const fmatch = lc.match(/^f(\d{1,2})$/);
  if (fmatch) return `F${fmatch[1]}`;
  // Anything else: punctuation like "," "/" ";". Keep as-is.
  return t;
}

export function parseShortcut(s: string | undefined | null): ParsedShortcut | null {
  if (!s) return null;
  const parts = s.split("+").map((p) => p.trim()).filter(Boolean);
  if (parts.length === 0) return null;
  let ctrl = false,
    alt = false,
    shift = false,
    meta = false;
  let key = "";
  for (const raw of parts) {
    const low = raw.toLowerCase();
    if (low === "ctrl" || low === "control") ctrl = true;
    else if (low === "alt" || low === "option" || low === "opt") alt = true;
    else if (low === "shift") shift = true;
    else if (low === "meta" || low === "cmd" || low === "command" || low === "win") meta = true;
    else key = normalizeKey(raw);
  }
  if (!key) return null;
  return { ctrl, alt, shift, meta, key };
}

export function matches(e: KeyboardEvent, accel: ParsedShortcut | null): boolean {
  if (!accel) return false;
  if (e.ctrlKey !== accel.ctrl) return false;
  if (e.altKey !== accel.alt) return false;
  if (e.shiftKey !== accel.shift) return false;
  if (e.metaKey !== accel.meta) return false;
  // event.key is upper-case for letters when Shift is held; comparing
  // against our normalised lower-case form would miss "C" vs "c".
  // Match either case.
  return e.key === accel.key || e.key.toLowerCase() === accel.key.toLowerCase();
}

/** Build a parsed-shortcut table from the current settings (with the
 *  defaults backfilled for any missing field). Returned at settings
 *  load and re-built on every settings:changed. */
export function buildShortcutTable(
  s: ShortcutsSettings | null | undefined,
): Record<keyof ShortcutsSettings, ParsedShortcut | null> {
  const merged: ShortcutsSettings = { ...DEFAULT_SHORTCUTS, ...(s ?? {}) };
  return {
    copy: parseShortcut(merged.copy),
    paste: parseShortcut(merged.paste),
    select_all: parseShortcut(merged.select_all),
    find: parseShortcut(merged.find),
    new_workspace: parseShortcut(merged.new_workspace),
    toggle_notes: parseShortcut(merged.toggle_notes),
    toggle_settings: parseShortcut(merged.toggle_settings),
    // copy_on_select_with_ctrl_c is a boolean toggle, not a parsed
    // shortcut. Carried in the table for shape consistency — callers
    // should check `settings.shortcuts.copy_on_select_with_ctrl_c`
    // directly rather than via this table.
    copy_on_select_with_ctrl_c: null,
  };
}

/** Format a KeyboardEvent as an accelerator string, used by the
 *  Settings UI's "click to record" picker. Returns null if the
 *  event has no non-modifier key (so the picker can keep listening). */
export function formatEvent(e: KeyboardEvent): string | null {
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Meta");
  // Exclude bare modifier keys.
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return null;
  let label = e.key;
  // Letters: uppercase for display ("Ctrl+Shift+C", not "Ctrl+Shift+c").
  if (label.length === 1 && label.match(/[a-z]/i)) label = label.toUpperCase();
  // Space → "Space" for readability.
  if (label === " ") label = "Space";
  parts.push(label);
  return parts.join("+");
}
