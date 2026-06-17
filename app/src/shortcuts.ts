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

// Phase 62.B (item G): the layout-INDEPENDENT key for an event, derived
// from `event.code` (the physical key). Returns a token comparable to
// normalizeKey() output — lowercase letter, digit, or punctuation char —
// or null for keys we don't map physically (named keys like Enter /
// ArrowUp, which are already layout-independent in `event.key`, so
// callers fall back to that). This is what makes letter / digit / punct
// shortcuts (copy, the STT push-to-talk hotkey, …) fire on non-US
// layouts, where `event.key` is the localized character — e.g. Hebrew
// "צ" for the physical M key, which previously never matched "m".
const CODE_PUNCT: Record<string, string> = {
  Equal: "=",
  Minus: "-",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
};
export function physicalKey(e: KeyboardEvent): string | null {
  const code = e.code;
  if (!code) return null;
  if (code.length === 4 && code.startsWith("Key")) return code[3].toLowerCase(); // KeyM → "m"
  if (code.length === 6 && code.startsWith("Digit")) return code[5]; // Digit5 → "5"
  if (code.startsWith("Numpad")) {
    const rest = code.slice(6);
    if (rest.length === 1 && rest >= "0" && rest <= "9") return rest; // Numpad5 → "5"
  }
  return CODE_PUNCT[code] ?? null;
}

/** Layout-independent single-key compare for HARDCODED shortcuts.
 *  Matches `key` (a normalized lowercase letter / digit / punct / named
 *  key) against BOTH the logical `event.key` and the physical
 *  `event.code`, so e.g. `keyEq(e, "p")` fires for the physical P key on
 *  a Hebrew layout (where `event.key` is "פ"). The caller checks
 *  modifiers. */
export function keyEq(e: KeyboardEvent, key: string): boolean {
  const k = key.toLowerCase();
  if (e.key.toLowerCase() === k) return true;
  const phys = physicalKey(e);
  return phys != null && phys.toLowerCase() === k;
}

export function matches(e: KeyboardEvent, accel: ParsedShortcut | null): boolean {
  if (!accel) return false;
  if (e.ctrlKey !== accel.ctrl) return false;
  if (e.altKey !== accel.alt) return false;
  if (e.shiftKey !== accel.shift) return false;
  if (e.metaKey !== accel.meta) return false;
  // Match the logical key (event.key, handles named keys + US layout)
  // OR the physical key (event.code, layout-independent). The physical
  // fallback is what makes a letter hotkey work on a Hebrew layout.
  return keyEq(e, accel.key);
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
    summarize_claude: parseShortcut(merged.summarize_claude),
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
  // Phase 62.B (item G): prefer the PHYSICAL key so recording the hotkey
  // on a non-US layout still stores the canonical accelerator (physical
  // M → "M", not the Hebrew "צ"). Named keys (Enter, ArrowUp…) have no
  // physical mapping → fall back to event.key.
  let label = physicalKey(e) ?? e.key;
  // Letters: uppercase for display ("Ctrl+Shift+C", not "Ctrl+Shift+c").
  if (label.length === 1 && label.match(/[a-z]/i)) label = label.toUpperCase();
  // Space → "Space" for readability.
  if (label === " ") label = "Space";
  parts.push(label);
  return parts.join("+");
}
