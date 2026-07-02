// Phase 9.A: settings type mirror + helpers (load/save/apply CSS vars).
// The Rust backend owns the canonical schema in src-tauri/src/settings.rs;
// this file is the typed mirror used by the frontend.

import { invoke } from "@tauri-apps/api/core";
import { setTerminalFont, setRtlMode, setAutoDirection, setAutoResetOnConnect, type RtlMode } from "./terminalInstance";

export interface AnsiPalette {
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  bright_black: string;
  bright_red: string;
  bright_green: string;
  bright_yellow: string;
  bright_blue: string;
  bright_magenta: string;
  bright_cyan: string;
  bright_white: string;
}

export interface Theme {
  preset: string;
  accent: string;
  background: string;
  surface: string;
  border: string;
  text_primary: string;
  text_secondary: string;
  success: string;
  warning: string;
  error: string;
  ansi: AnsiPalette;
}

export interface FontSettings {
  ui_family: string;
  ui_size_pt: number;
  terminal_family: string;
  terminal_size_pt: number;
  /** Stretch goal: load a web font sheet (e.g. Google Fonts) at runtime. */
  web_font_url?: string | null;
}

export interface TerminalSettings {
  cursor_style: "block" | "bar" | "underline";
  scrollback_lines: number;
  bidi_enabled: boolean;
  allow_proposed_api: boolean;
  /** Phase 15.A: how to render Hebrew / Arabic. */
  rtl_mode?: "auto_per_line" | "bidi_reorder" | "off";
  /** Phase tmux-conf: when true (default), winmux launches tmux with
   *  `-f ~/.winmux/tmux.conf` for sane scrollback / mouse behaviour.
   *  Set false to fall back to the user's own ~/.tmux.conf. */
  use_winmux_tmux_config?: boolean;
  /** Phase HH: mirror Left/Right arrows on RTL (Hebrew/Arabic) lines.
   *  Only active when the cursor's line is RTL; default true. */
  mirror_arrows_rtl?: boolean;
  /** v0.4.4 (RTL Approach C): auto-flip each terminal line's direction
   *  (mixed/pure-Hebrew → RTL, pure-Latin → LTR). Only affects the
   *  `auto_per_line` rtl_mode. Default true. */
  auto_direction?: boolean;
  /** v0.4.4-beta.2: clear stale mouse-tracking modes on connect (fixes the
   *  `\e[<..M` mouse-escape leak from an unclean vim/fzf/less exit).
   *  Default true. */
  auto_reset_on_connect?: boolean;
}

export interface HooksSettings {
  enabled: boolean;
  agents: string[];
  policy_preset: string;
  /** Phase 18.1: which PreToolUse matcher to install when setup-hooks
   *  runs. "restrictive" (default) only catches risky tools; "all"
   *  matches `.*` (every tool surfaces a winmux card); "custom" leaves
   *  whatever the user hand-edited and never overwrites. */
  matcher_mode?: "restrictive" | "all" | "custom";
  /** Phase 66 (66.D): master switch for the 3-state policy engine
   *  (auto/gate/block) in the desktop feed.push handler. Default true. */
  policy_enabled?: boolean;
  /** Phase 66 (66.B): auto-run `winmux setup-hooks` on the remote during
   *  bootstrap so a fresh server starts surfacing cards. Default true. */
  auto_install?: boolean;
}

// beta.3: canonical Claude Code hook types. Wire strings are kebab-case
// (round-trips with rpc_server.rs subkind handling).
export type HookType =
  | "pre-tool-use"
  | "notification"
  | "stop"
  | "session-end"
  | "post-tool-use"
  | "subagent-stop"
  | "user-prompt-submit"
  | "pre-compact"
  | "session-start";

// beta.3: per-hook-type "processed?" + "play sound?" toggles + master.
// Mirrors the Rust `HookSettings` struct in settings.rs. Ts-rs also emits
// a binding under app/src/bindings/HookSettings.ts; this hand-mirrored
// type is what the app imports because HashSet<T> serialises as an array
// and we want a plain TS shape for the UI.
export interface HookNotificationSettings {
  enabled_types: HookType[];
  sound_types: HookType[];
  sound_master: boolean;
}

export const INTERACTIVE_HOOKS: HookType[] = [
  "pre-tool-use",
  "notification",
  "stop",
  "session-end",
];

export const OBSERVABILITY_HOOKS: HookType[] = [
  "post-tool-use",
  "subagent-stop",
  "user-prompt-submit",
  "pre-compact",
  "session-start",
];

export const DEFAULT_HOOK_NOTIFICATIONS: HookNotificationSettings = {
  enabled_types: [...INTERACTIVE_HOOKS],
  sound_types: ["pre-tool-use", "notification", "stop"],
  sound_master: true,
};

export interface NotificationSettings {
  toast_enabled: boolean;
  sound_enabled: boolean;
  /** Phase 66 (KK): per-event toast toggles. Defaults: session start/end
   *  OFF; stop / notification / gate / block ON. */
  toast_session_start?: boolean;
  toast_session_end?: boolean;
  toast_stop?: boolean;
  toast_notification?: boolean;
  toast_gate?: boolean;
  toast_block?: boolean;
  /** cmux-A A1: pulse a pane's border on OSC 9/99/777. Default true. */
  pane_pulse_on_activity?: boolean;
}

export interface UpdatesSettings {
  check_on_startup: boolean;
  auto_download: boolean;
  manifest_url?: string | null;
  last_check_iso?: string | null;
  last_seen_version?: string | null;
  skipped_versions: string[];
  remind_after_iso?: string | null;
  // Phase 71: "stable" | "beta".
  channel: string;
}

export interface ShortcutsSettings {
  copy: string;
  paste: string;
  select_all: string;
  find: string;
  new_workspace: string;
  toggle_notes: string;
  toggle_settings: string;
  summarize_claude: string;
  copy_on_select_with_ctrl_c: boolean;
}

export const DEFAULT_SHORTCUTS: ShortcutsSettings = {
  copy: "Ctrl+Shift+C",
  paste: "Ctrl+Shift+V",
  select_all: "Ctrl+Shift+A",
  find: "Ctrl+F",
  new_workspace: "Ctrl+N",
  toggle_notes: "Ctrl+Shift+N",
  toggle_settings: "Ctrl+,",
  summarize_claude: "Ctrl+Alt+B",
  copy_on_select_with_ctrl_c: true,
};

export interface ClaudeSettings {
  auto_summarize_on_stop: boolean;
  summary_history_count: number;
  summary_prompt: string;
  summary_language: string;
}

export interface HooksUpdatesSettings {
  show_banners: boolean;
  /** Map of agent_id → list of dismissed version strings. */
  dismissed: Record<string, string[]>;
}

export const DEFAULT_HOOKS_UPDATES: HooksUpdatesSettings = {
  show_banners: true,
  dismissed: {},
};

export interface HooksOutdatedInfo {
  workspace_id: string;
  pane_id: string;
  agent: string;
  current?: string | null;
  latest: string;
}

export const DEFAULT_CLAUDE_SETTINGS: ClaudeSettings = {
  auto_summarize_on_stop: false,
  summary_history_count: 10,
  summary_prompt:
    "Summarize the last {N} exchanges in 2-3 sentences in the same language the conversation used.",
  summary_language: "auto",
};

// Phase 78: Claude subscription-usage indicator (mirrors the Rust
// ClaudeUsageSettings struct in app/src-tauri/src/settings.rs).
export interface ClaudeUsageSettings {
  show_top_indicator: boolean;
  display_mode: "percent" | "bar" | string;
  auto_refresh_minutes: number;
}

export const DEFAULT_CLAUDE_USAGE_SETTINGS: ClaudeUsageSettings = {
  show_top_indicator: true,
  display_mode: "percent",
  auto_refresh_minutes: 10,
};

export interface I18nSettings {
  language: "en" | "he" | "ar" | "ru" | string;
  direction: "auto" | "ltr" | "rtl" | string;
}

// Phase 58: speech-to-text settings (hand-mirrored from the Rust
// SttSettings struct in app/src-tauri/src/settings.rs). When the Rust
// side regenerates app/src/bindings/SttSettings.ts via ts-rs the
// types should stay structurally identical; this file is what the
// rest of the frontend imports historically, so we add the type
// here too.
export interface SttSettings {
  enabled: boolean;
  backend: "webspeech" | "local";
  local_endpoint: string | null;
  language: string;
  push_to_talk_hotkey: string;
}

export interface Settings {
  version: number;
  theme: Theme;
  font: FontSettings;
  terminal: TerminalSettings;
  hooks: HooksSettings;
  notifications: NotificationSettings;
  // beta.3: per-hook-type enable + sound toggles (Hooks & Notifications card).
  hook_notifications?: HookNotificationSettings;
  updates: UpdatesSettings;
  i18n: I18nSettings;
  shortcuts?: ShortcutsSettings;
  claude?: ClaudeSettings;
  // Phase 78: Claude usage % indicator display + auto-refresh.
  claude_usage?: ClaudeUsageSettings;
  hooks_updates?: HooksUpdatesSettings;
  // Phase 41: auto-connect a background SSH session on workspace select.
  // Backend defaults to true; always serialized.
  auto_connect_on_workspace_select?: boolean;
  // Phase 49-C: optional auto-delete of empty workspaces older than N
  // days. null/undefined = disabled. Range 1-90 enforced by the UI.
  auto_destroy_empty_workspaces_days?: number | null;
  // Phase 58: voice input (speech-to-text) — opt-in. Defaults via
  // serde(default) on the Rust side, so older settings.json files
  // load with stt: { enabled: false, backend: "webspeech", ... }.
  stt?: SttSettings;
  // Phase 62.B (item I): sidebar display mode. Backend defaults to
  // "full" via serde(default). Phase 65.P: two modes only (full /
  // icons) — the old "hidden" value migrates to "icons" on read.
  sidebar_mode?: SidebarMode;
  // Phase 63: per-kind floating-window state (Browser / FileManager).
  floating_windows?: FloatingWindows;
  // Phase 75: debug-log retention.
  logs?: LogsSettings;
  // Unshipped-fivefer (#3): persist workspace-browser sessions (cookies/
  // logins) across restarts. Backend defaults to true.
  persist_browser_sessions?: boolean;
  // Design Pass 01 (#2): dark/light appearance axis. Backend defaults to
  // "system" via serde(default); older settings.json load unchanged.
  theme_mode?: ThemeMode;
}

// Design Pass 01 (#2): appearance polarity. "system" follows the OS.
export type ThemeMode = "dark" | "light" | "system";

// Phase 75: debug.log hygiene.
export interface LogsSettings {
  retention_days: number;
}

// Phase 65.P: dropped "hidden" — only full / icons. Old persisted
// "hidden" values are migrated to "icons" at read time (App.tsx).
export type SidebarMode = "full" | "icons";

// Phase 63: 3-mode floating windows.
export type FloatingWindowMode = "pane" | "float" | "popout";
export interface FloatingRect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export interface FloatingWindowState {
  mode?: FloatingWindowMode;
  float_rect?: FloatingRect | null;
  popout_rect?: FloatingRect | null;
  popout_display?: number | null;
  pane_width?: number | null;
}
export interface FloatingWindows {
  browser?: FloatingWindowState;
  filemanager?: FloatingWindowState;
}

export interface SummaryResult {
  text: string;
  session_id: string;
  messages_count: number;
  generated_at: string;
  note_id?: string | null;
}

export interface PresetEntry {
  id: string;
  label: string;
  theme: Theme;
}

export interface FontFamilies {
  ui: string[];
  mono: string[];
}

export interface UpdateInfo {
  current_version: string;
  latest_version?: string | null;
  available: boolean;
  notes_url?: string | null;
  msi_url?: string | null;
  released_at?: string | null;
  manifest_url?: string | null;
  error?: string | null;
  last_check_iso: string;
}

// ─── disk I/O via Tauri commands ─────────────────────────────────────────

export const loadSettings = (): Promise<Settings> =>
  invoke<Settings>("settings_load");

export const saveSettings = (settings: Settings): Promise<Settings> =>
  invoke<Settings>("settings_save", { settings });

export const getPresets = (): Promise<PresetEntry[]> =>
  invoke<PresetEntry[]>("settings_get_presets");

export const applyPreset = (preset: string): Promise<Settings> =>
  invoke<Settings>("settings_apply_preset", { preset });

export const resetSettings = (): Promise<Settings> =>
  invoke<Settings>("settings_reset");

export const listSystemFonts = (): Promise<FontFamilies> =>
  invoke<FontFamilies>("list_system_fonts");

export const checkForUpdates = (): Promise<UpdateInfo> =>
  invoke<UpdateInfo>("check_for_updates_now");

// ─── theme apply ─────────────────────────────────────────────────────────

/**
 * Write the current theme into CSS variables on `<html>`. App.css reads
 * them (var(--w-bg) etc.) so the entire UI re-tints instantly. Called on
 * startup after load and on every `settings:changed` event.
 */
export function applyTheme(s: Settings): void {
  const r = document.documentElement.style;
  const t = s.theme;
  r.setProperty("--w-bg", t.background);
  r.setProperty("--w-surface", t.surface);
  r.setProperty("--w-border", t.border);
  r.setProperty("--w-text", t.text_primary);
  r.setProperty("--w-text-dim", t.text_secondary);
  r.setProperty("--w-accent", t.accent);
  r.setProperty("--w-success", t.success);
  r.setProperty("--w-warning", t.warning);
  r.setProperty("--w-error", t.error);
  // Derive a couple of secondary tones from the base ones rather than
  // requiring users to set all of them.
  r.setProperty("--w-surface-hi", mix(t.surface, t.text_primary, 0.06));
  r.setProperty("--w-border-hi", mix(t.border, t.text_primary, 0.1));
  r.setProperty("--w-text-faint", mix(t.text_secondary, t.background, 0.4));
  r.setProperty("--w-accent-hi", mix(t.accent, "#ffffff", 0.18));

  r.setProperty("--w-font-ui", quoteFamily(s.font.ui_family));
  r.setProperty("--w-font-mono", quoteFamily(s.font.terminal_family));
  // Phase 9.A live size apply. App.css now bases :root font-size on this
  // var, and the --w-fs-* size vars are in em — so changing this single pt
  // value rescales every UI element proportionally.
  r.setProperty("--w-font-size-ui", `${s.font.ui_size_pt}pt`);
  // Push terminal font + size into every live xterm instance. New panes
  // opened later inherit the cached values via the constructor.
  setTerminalFont(quoteFamily(s.font.terminal_family), s.font.terminal_size_pt);
  // Phase 15.A: push the RTL mode. The write pipeline flips immediately
  // on every live pane; the renderer choice (DOM vs WebGL) is sticky
  // per pane and only affects newly-opened terminals.
  const mode = (s.terminal.rtl_mode ?? "auto_per_line") as RtlMode;
  setRtlMode(mode);
  // v0.4.4: per-line auto-direction escape hatch (default on).
  setAutoDirection(s.terminal.auto_direction ?? true);
  // v0.4.4-beta.2: clear stale mouse-tracking modes on connect (default on).
  setAutoResetOnConnect(s.terminal.auto_reset_on_connect ?? true);

  // Design Pass 01 (#2): dark/light axis. Resolve "system" against the OS
  // and write data-theme-mode on <html>; tokens.css keys the Light chrome
  // palette off it. Independent of the colour preset.
  document.documentElement.dataset.themeMode = resolveThemeMode(s.theme_mode);

  // Phase font-bug-fix v2 (stretch): if a web font URL is configured,
  // inject a single <link rel="stylesheet"> tag so that font becomes
  // available by family name. Removing or changing the URL replaces the
  // tag — we don't try to garbage-collect previously-loaded sheets.
  const url = (s.font as any).web_font_url as string | undefined;
  applyWebFont(url ?? "");
}

/**
 * Design Pass 01 (#2): resolve the appearance axis to a concrete polarity.
 * "system" (or a missing value) follows the OS `prefers-color-scheme`.
 */
export function resolveThemeMode(mode: string | undefined): "dark" | "light" {
  if (mode === "light") return "light";
  if (mode === "dark") return "dark";
  return window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

/**
 * Re-apply the theme when the OS scheme flips while the user is on
 * "system". Registered once at startup with a live settings getter.
 */
export function watchSystemTheme(getSettings: () => Settings): void {
  const mq = window.matchMedia?.("(prefers-color-scheme: light)");
  if (!mq) return;
  mq.addEventListener("change", () => {
    if ((getSettings().theme_mode ?? "system") === "system") {
      applyTheme(getSettings());
    }
  });
}

function applyWebFont(url: string): void {
  const existing = document.getElementById("winmux-web-font") as
    | HTMLLinkElement
    | null;
  const trimmed = (url || "").trim();
  if (!trimmed) {
    if (existing) existing.remove();
    return;
  }
  // Don't reload the same URL.
  if (existing && existing.href === trimmed) return;
  if (existing) existing.remove();
  const link = document.createElement("link");
  link.id = "winmux-web-font";
  link.rel = "stylesheet";
  link.href = trimmed;
  link.crossOrigin = "anonymous";
  document.head.appendChild(link);
}

function quoteFamily(family: string): string {
  // Wrap with single quotes if the family has a space and isn't already
  // quoted; append safe fallbacks so a missing font doesn't break layout.
  const trimmed = family.trim();
  const isMono =
    /mono|consolas|cascadia|courier|menlo|fira|jetbrains|iosevka|hack|source code|lucida console/i.test(
      trimmed
    );
  const head = trimmed && !/[",']/.test(trimmed) && /\s/.test(trimmed)
    ? `"${trimmed}"`
    : trimmed;
  const fallback = isMono
    ? '"Cascadia Mono", "JetBrains Mono", Consolas, ui-monospace, monospace'
    : '-apple-system, "Segoe UI Variable", "Segoe UI", system-ui, sans-serif';
  return `${head}, ${fallback}`;
}

// Minimal hex color blender (#rrggbb only). Best-effort — non-hex values
// pass through unchanged, which still works because CSS will fall back
// when it sees an invalid value.
function mix(base: string, with_: string, amount: number): string {
  const a = parseHex(base);
  const b = parseHex(with_);
  if (!a || !b) return base;
  const t = Math.max(0, Math.min(1, amount));
  const m = (i: number) => Math.round(a[i] * (1 - t) + b[i] * t);
  return `rgb(${m(0)}, ${m(1)}, ${m(2)})`;
}

function parseHex(c: string): [number, number, number] | null {
  const s = c.trim().replace(/^#/, "");
  if (s.length === 3) {
    const r = parseInt(s[0] + s[0], 16);
    const g = parseInt(s[1] + s[1], 16);
    const b = parseInt(s[2] + s[2], 16);
    if ([r, g, b].some((v) => Number.isNaN(v))) return null;
    return [r, g, b];
  }
  if (s.length === 6) {
    const r = parseInt(s.slice(0, 2), 16);
    const g = parseInt(s.slice(2, 4), 16);
    const b = parseInt(s.slice(4, 6), 16);
    if ([r, g, b].some((v) => Number.isNaN(v))) return null;
    return [r, g, b];
  }
  return null;
}
