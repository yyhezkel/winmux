//! Phase 9.A: app settings (theme + font + terminal + hooks + notifications +
//! updates). Persisted in `%APPDATA%\winmux\settings.json` next to
//! `workspaces.json` / `notes.json`. Same atomic-write + load-poison-gate
//! pattern. Mutations emit `settings:changed` to the frontend so live theme
//! updates from the CLI reflect into the UI without a reload.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, dlog, AppState};

// ─── beta.3: hook-type enum + per-hook enable/sound settings ───────────────

/// beta.3: canonical list of Claude Code hook types. Serialized in the
/// kebab-case wire form ("pre-tool-use" etc.) so it round-trips with the
/// existing hook `subkind` strings the CLI emits (see rpc_server.rs).
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Hash, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookType {
    PreToolUse,
    Notification,
    Stop,
    SessionEnd,
    PostToolUse,
    SubagentStop,
    UserPromptSubmit,
    PreCompact,
    SessionStart,
}

/// beta.3: per-hook toggles: which types the backend actually processes and
/// which of those play a sound on the toast.
///
/// Migration policy (see `default_hook_notifications` / `migrate_settings`):
/// when an older settings.json has no `hook_notifications` object, the
/// defaults kick in — the interactive-4 (PreToolUse / Notification / Stop /
/// SessionEnd) are enabled; the interactive-3 (PreToolUse / Notification /
/// Stop) additionally get sound; sound_master starts on. The verbose
/// observability hooks (PostToolUse / SubagentStop / UserPromptSubmit /
/// PreCompact / SessionStart) are off across the board.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct HookSettings {
    #[serde(default = "default_enabled_types")]
    pub enabled_types: HashSet<HookType>,
    #[serde(default = "default_sound_types")]
    pub sound_types: HashSet<HookType>,
    #[serde(default = "default_true")]
    pub sound_master: bool,
}

fn default_enabled_types() -> HashSet<HookType> {
    let mut s = HashSet::new();
    s.insert(HookType::PreToolUse);
    s.insert(HookType::Notification);
    s.insert(HookType::Stop);
    s.insert(HookType::SessionEnd);
    s
}

fn default_sound_types() -> HashSet<HookType> {
    let mut s = HashSet::new();
    s.insert(HookType::PreToolUse);
    s.insert(HookType::Notification);
    s.insert(HookType::Stop);
    s
}

impl Default for HookSettings {
    fn default() -> Self {
        Self {
            enabled_types: default_enabled_types(),
            sound_types: default_sound_types(),
            sound_master: true,
        }
    }
}

/// beta.3: convert a wire subkind ("pre-tool-use", "notification", …) to the
/// enum. Returns None for the retired `session-start` on legacy CLI 1.1.0 or
/// anything unknown.
pub(crate) fn hook_type_from_subkind(s: &str) -> Option<HookType> {
    match s {
        "pre-tool-use" => Some(HookType::PreToolUse),
        "notification" => Some(HookType::Notification),
        "stop" => Some(HookType::Stop),
        "session-end" => Some(HookType::SessionEnd),
        "post-tool-use" => Some(HookType::PostToolUse),
        "subagent-stop" => Some(HookType::SubagentStop),
        "user-prompt-submit" => Some(HookType::UserPromptSubmit),
        "pre-compact" => Some(HookType::PreCompact),
        "session-start" => Some(HookType::SessionStart),
        _ => None,
    }
}

// ─── data model ────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct AnsiPalette {
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl AnsiPalette {
    fn tokyo_night() -> Self {
        Self {
            black: "#15161e".into(),
            red: "#f7768e".into(),
            green: "#9ece6a".into(),
            yellow: "#e0af68".into(),
            blue: "#7aa2f7".into(),
            magenta: "#bb9af7".into(),
            cyan: "#7dcfff".into(),
            white: "#a9b1d6".into(),
            bright_black: "#414868".into(),
            bright_red: "#ff7a93".into(),
            bright_green: "#b9f27c".into(),
            bright_yellow: "#ff9e64".into(),
            bright_blue: "#7da6ff".into(),
            bright_magenta: "#bb9af7".into(),
            bright_cyan: "#0db9d7".into(),
            bright_white: "#c0caf5".into(),
        }
    }
    fn dracula() -> Self {
        Self {
            black: "#21222c".into(),
            red: "#ff5555".into(),
            green: "#50fa7b".into(),
            yellow: "#f1fa8c".into(),
            blue: "#bd93f9".into(),
            magenta: "#ff79c6".into(),
            cyan: "#8be9fd".into(),
            white: "#f8f8f2".into(),
            bright_black: "#6272a4".into(),
            bright_red: "#ff6e6e".into(),
            bright_green: "#69ff94".into(),
            bright_yellow: "#ffffa5".into(),
            bright_blue: "#d6acff".into(),
            bright_magenta: "#ff92df".into(),
            bright_cyan: "#a4ffff".into(),
            bright_white: "#ffffff".into(),
        }
    }
    fn solarized_dark() -> Self {
        Self {
            black: "#073642".into(),
            red: "#dc322f".into(),
            green: "#859900".into(),
            yellow: "#b58900".into(),
            blue: "#268bd2".into(),
            magenta: "#d33682".into(),
            cyan: "#2aa198".into(),
            white: "#eee8d5".into(),
            bright_black: "#002b36".into(),
            bright_red: "#cb4b16".into(),
            bright_green: "#586e75".into(),
            bright_yellow: "#657b83".into(),
            bright_blue: "#839496".into(),
            bright_magenta: "#6c71c4".into(),
            bright_cyan: "#93a1a1".into(),
            bright_white: "#fdf6e3".into(),
        }
    }
    fn nord() -> Self {
        Self {
            black: "#3b4252".into(),
            red: "#bf616a".into(),
            green: "#a3be8c".into(),
            yellow: "#ebcb8b".into(),
            blue: "#81a1c1".into(),
            magenta: "#b48ead".into(),
            cyan: "#88c0d0".into(),
            white: "#e5e9f0".into(),
            bright_black: "#4c566a".into(),
            bright_red: "#bf616a".into(),
            bright_green: "#a3be8c".into(),
            bright_yellow: "#ebcb8b".into(),
            bright_blue: "#81a1c1".into(),
            bright_magenta: "#b48ead".into(),
            bright_cyan: "#8fbcbb".into(),
            bright_white: "#eceff4".into(),
        }
    }
    fn solarized_light() -> Self {
        Self {
            black: "#073642".into(),
            red: "#dc322f".into(),
            green: "#859900".into(),
            yellow: "#b58900".into(),
            blue: "#268bd2".into(),
            magenta: "#d33682".into(),
            cyan: "#2aa198".into(),
            white: "#eee8d5".into(),
            bright_black: "#002b36".into(),
            bright_red: "#cb4b16".into(),
            bright_green: "#586e75".into(),
            bright_yellow: "#657b83".into(),
            bright_blue: "#839496".into(),
            bright_magenta: "#6c71c4".into(),
            bright_cyan: "#93a1a1".into(),
            bright_white: "#fdf6e3".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Theme {
    pub preset: String,
    pub accent: String,
    pub background: String,
    pub surface: String,
    pub border: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub ansi: AnsiPalette,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Font {
    pub ui_family: String,
    pub ui_size_pt: u32,
    pub terminal_family: String,
    pub terminal_size_pt: u32,
    /// Stretch goal: optional URL to a CSS stylesheet (e.g. Google Fonts)
    /// that the frontend injects via <link rel="stylesheet"> so the user
    /// can pick a non-installed family and have it fetched at runtime.
    /// Empty / None = no extra fonts loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_font_url: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct TerminalSettings {
    pub cursor_style: String,
    pub scrollback_lines: u32,
    pub bidi_enabled: bool,
    pub allow_proposed_api: bool,
    /// Phase 15.A: how to handle Hebrew / Arabic in the terminal.
    /// One of "auto_per_line" (default, Termius-style — DOM renderer
    /// + dir="auto" on every row), "bidi_reorder" (legacy v1, WebGL +
    /// bidi-js logical→visual reorder), or "off" (WebGL, no reorder).
    /// New panes pick up the renderer immediately; live mode swaps
    /// affect the reorder pipeline on currently-open panes.
    #[serde(default = "default_rtl_mode")]
    pub rtl_mode: String,
    /// Phase tmux-conf: when true (default), tmux is launched with
    /// `-f ~/.winmux/tmux.conf` so the bundled scrollback-friendly
    /// config applies (wheel scrolls the scrollback ring instead of
    /// shell history, 50k-line buffer, mouse on, sane truecolour).
    /// Set false to fall back to the user's own `~/.tmux.conf`. The
    /// conf file is uploaded by the bootstrap regardless, so the
    /// toggle takes effect on the NEXT pane connect.
    #[serde(default = "default_true")]
    pub use_winmux_tmux_config: bool,
    /// Phase HH: mirror the physical Left/Right arrow keys when the
    /// terminal line under the cursor is right-to-left (Hebrew/Arabic).
    /// In an RTL line the visual "right" is logical "left", so without
    /// this the arrows feel inverted. Only takes effect on RTL lines —
    /// LTR lines are unaffected — so it's safe to leave on (default true).
    #[serde(default = "default_true")]
    pub mirror_arrows_rtl: bool,
    /// v0.4.4 (RTL Approach C): auto-flip each terminal line's paragraph
    /// direction from its text — a line with any Hebrew/Arabic char renders
    /// RTL (mixed or pure), a pure-Latin line renders LTR. Only affects the
    /// `auto_per_line` rtl_mode. Default true; set false for classic
    /// LTR-only terminal behaviour.
    #[serde(default = "default_true")]
    pub auto_direction: bool,
    /// v0.4.4-beta.2: on connect/attach, clear stale mouse-tracking modes an
    /// unclean app exit (vim/fzf/less/htop killed) can leave on — which makes
    /// the bare shell print `\e[<..M` mouse escapes as text. Default true; a
    /// manual "Reset terminal" (Ctrl+Alt+R) is always available regardless.
    #[serde(default = "default_true")]
    pub auto_reset_on_connect: bool,
}

fn default_rtl_mode() -> String {
    "auto_per_line".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Hooks {
    pub enabled: bool,
    pub agents: Vec<String>,
    pub policy_preset: String,
    /// Phase 18.1: which PreToolUse matcher to install in the agent's
    /// settings.json. `"restrictive"` (default) only matches risky tools
    /// (`Bash|Write|Edit|MultiEdit|NotebookEdit|Task`); `"all"` matches
    /// every tool (`.*`) so EVERY action surfaces a winmux card; `"custom"`
    /// keeps whatever the user hand-edited locally and is never overwritten
    /// by `winmux setup-hooks`. The setting is consumed by the desktop's
    /// remote-side setup-hooks call (Phase 18 wraps `agent.setup_hooks`).
    #[serde(default = "default_matcher_mode")]
    pub matcher_mode: String,
    /// Phase 66 (66.D): master switch for the 3-state policy engine
    /// (auto / gate / block) that runs in the desktop `feed.push` handler.
    /// When false, every pre-tool-use request becomes a blocking card (the
    /// pre-66 behavior). Default true. Older settings.json without the
    /// field loads with the engine ON.
    #[serde(default = "default_true")]
    pub policy_enabled: bool,
    /// Phase 66 (66.B): when true (default), the SSH bootstrap auto-runs
    /// `winmux setup-hooks` on the remote after deploying the CLI, so a
    /// fresh server starts surfacing permission cards without the user
    /// invoking setup-hooks by hand. No-op if Claude Code isn't installed
    /// remotely. Older settings.json loads with auto-install ON.
    #[serde(default = "default_true")]
    pub auto_install: bool,
}

fn default_matcher_mode() -> String {
    "restrictive".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Notifications {
    /// Master switch — when false, no hook toasts at all.
    pub toast_enabled: bool,
    pub sound_enabled: bool,
    // Phase 66 (KK): per-event toast toggles. Defaults chosen to cut noise
    // — lifecycle session events are silent; "needs you" / "finished" /
    // security events surface. Older settings.json loads with these
    // serde defaults (so an upgrade picks the sane set automatically).
    /// Claude session started — noisy, default OFF.
    #[serde(default)]
    pub toast_session_start: bool,
    /// Claude session ended — default OFF.
    #[serde(default)]
    pub toast_session_end: bool,
    /// Claude finished a task (Stop) — useful, default ON.
    #[serde(default = "default_true")]
    pub toast_stop: bool,
    /// Claude needs you (Notification event) — critical, default ON.
    #[serde(default = "default_true")]
    pub toast_notification: bool,
    /// A tool needs approval (PreToolUse gate) — must respond, default ON.
    #[serde(default = "default_true")]
    pub toast_gate: bool,
    /// A dangerous tool was blocked — security insight, default ON.
    #[serde(default = "default_true")]
    pub toast_block: bool,
    /// cmux-A A1: pulse a pane's border when an OSC 9/99/777 terminal
    /// notification arrives for it. Cleared when the user focuses the
    /// pane. Default ON — degrades to a static solid ring under
    /// prefers-reduced-motion: reduce.
    #[serde(default = "default_true")]
    pub pane_pulse_on_activity: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Updates {
    pub check_on_startup: bool,
    pub auto_download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_iso: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_version: Option<String>,
    /// Phase 65 (U): versions the user chose to skip — the
    /// `update:available` banner stays suppressed for these until a
    /// newer version appears. Older settings.json without this field
    /// load with an empty list.
    #[serde(default)]
    pub skipped_versions: Vec<String>,
    /// Phase 65 (U): "remind me later" — suppress the banner until this
    /// ISO timestamp passes. None = no active snooze.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_after_iso: Option<String>,
    /// Phase 71: update channel — "stable" (only `MAJOR.MINOR.PATCH`
    /// releases) or "beta" (also shows pre-releases like `0.4.0-beta1`).
    /// Older settings.json without this field default to stable.
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_channel() -> String {
    "stable".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct I18n {
    pub language: String,
    pub direction: String,
}

impl Default for I18n {
    fn default() -> Self {
        Self {
            language: "en".into(),
            direction: "auto".into(),
        }
    }
}

/// Phase 16: configurable keyboard shortcuts. Stored as human-readable
/// `Ctrl+Shift+X` strings — parsed in the frontend (see
/// `src/shortcuts.ts`) so users can hand-edit settings.json and the
/// next launch picks up the change.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Shortcuts {
    pub copy: String,
    pub paste: String,
    pub select_all: String,
    pub find: String,
    pub new_workspace: String,
    pub toggle_notes: String,
    pub toggle_settings: String,
    /// Phase 17: trigger a manual Claude session summary. Default
    /// Ctrl+Alt+B (B for "brief"). #[serde(default)] so pre-17
    /// settings.json files don't need to be touched.
    #[serde(default = "default_summarize_claude")]
    pub summarize_claude: String,
    /// When true and the terminal has a selection, plain Ctrl+C copies
    /// to clipboard instead of sending SIGINT. Matches Windows Terminal
    /// + most modern terminal apps. Set to false to always send SIGINT.
    #[serde(default = "default_true")]
    pub copy_on_select_with_ctrl_c: bool,
}

fn default_summarize_claude() -> String {
    "Ctrl+Alt+B".to_string()
}

/// Phase 17: Claude-specific options.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeOptions {
    pub auto_summarize_on_stop: bool,
    pub summary_history_count: u32,
    pub summary_prompt: String,
    /// `"auto"` lets the prompt itself control language (default).
    /// A specific ISO code (`"he"`, `"en"`) appends a hint like
    /// "Respond in Hebrew." to the prompt. Frontend currently just
    /// surfaces "auto" — the field is here for future expansion.
    pub summary_language: String,
}

impl Default for ClaudeOptions {
    fn default() -> Self {
        Self {
            auto_summarize_on_stop: false,
            summary_history_count: 10,
            summary_prompt: "Summarize the last {N} exchanges in 2-3 sentences in the same language the conversation used.".to_string(),
            summary_language: "auto".into(),
        }
    }
}

/// Phase 78: Claude subscription-usage display options. The usage data
/// itself comes from `claude -p "/usage"` over SSH (see claude_usage.rs);
/// these settings only control how the global % indicator is shown and how
/// often a live connection auto-refreshes it.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeUsageSettings {
    /// Show the compact usage % indicator at the top of the sidebar.
    pub show_top_indicator: bool,
    /// `"percent"` (colored NN% text) or `"bar"`. A String (not an enum) to
    /// match the sidebar_mode / rtl_mode pattern → plain TS union.
    pub display_mode: String,
    /// Auto-refresh cadence for the active *live* (non-headless) workspace,
    /// in minutes. `0` = off (manual refresh only). The calls are free, so a
    /// modest interval keeps the indicator fresh without user action.
    pub auto_refresh_minutes: u32,
}

impl Default for ClaudeUsageSettings {
    fn default() -> Self {
        Self {
            show_top_indicator: true,
            display_mode: "percent".into(),
            auto_refresh_minutes: 10,
        }
    }
}

/// Phase 18: per-user state for the hooks-outdated banner. Tracked
/// separately from `Hooks` (which is per-agent enablement) because
/// the dismiss list belongs to the UI layer, not to the hook spec.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct HooksUpdates {
    pub show_banners: bool,
    /// `agent → [version-strings-the-user-said-skip]`. Empty entries
    /// are tolerated so a Clear-from-Settings can keep the agent key
    /// around without re-listing every dismissed version.
    pub dismissed: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for HooksUpdates {
    fn default() -> Self {
        Self {
            show_banners: true,
            dismissed: Default::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            copy: "Ctrl+Shift+C".into(),
            paste: "Ctrl+Shift+V".into(),
            select_all: "Ctrl+Shift+A".into(),
            find: "Ctrl+F".into(),
            new_workspace: "Ctrl+N".into(),
            toggle_notes: "Ctrl+Shift+N".into(),
            toggle_settings: "Ctrl+,".into(),
            summarize_claude: default_summarize_claude(),
            copy_on_select_with_ctrl_c: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Settings {
    pub version: u32,
    pub theme: Theme,
    pub font: Font,
    pub terminal: TerminalSettings,
    pub hooks: Hooks,
    pub notifications: Notifications,
    pub updates: Updates,
    // Phase 12.A — defaults to en/auto. `#[serde(default)]` so older
    // settings.json files load without the field.
    #[serde(default)]
    pub i18n: I18n,
    /// Phase 16. `#[serde(default)]` so pre-16 settings.json files
    /// load with the built-in defaults.
    #[serde(default)]
    pub shortcuts: Shortcuts,
    /// Phase 17. Claude session summary options.
    #[serde(default)]
    pub claude: ClaudeOptions,
    /// Phase 78. Claude subscription-usage indicator display + auto-refresh.
    #[serde(default)]
    pub claude_usage: ClaudeUsageSettings,
    /// Phase 18. Hooks-outdated banner show/skip state.
    #[serde(default)]
    pub hooks_updates: HooksUpdates,
    /// Phase 32.B. When true, suppress the "set up SSH key
    /// authentication?" offer after a password-auth connect. Persisted
    /// when the user ticks "Don't show again" in the offer modal.
    /// `#[serde(default)]` so older settings.json loads cleanly.
    #[serde(default)]
    pub ssh_key_offer_disabled: bool,
    /// Phase 41. When true (default), activating an SSH workspace
    /// establishes a background SSH session so the tmux session picker and
    /// the remote file manager populate without the user opening a
    /// terminal pane first. Disable to defer the connection until a pane
    /// connects. `default = "default_true"` keeps pre-41 settings.json
    /// backwards-compatible (missing field → true).
    #[serde(default = "default_true")]
    pub auto_connect_on_workspace_select: bool,
    /// Phase 49-C: optional auto-delete of empty + stale workspaces at
    /// startup. `None` (default) = disabled. Range 1-90 days enforced
    /// by the UI; the backend sweep treats any non-zero positive value
    /// as a valid TTL. A workspace is "empty" for sweep purposes when
    /// it has no live SSH sessions and its `last_active_at` is older
    /// than the TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_destroy_empty_workspaces_days: Option<u32>,
    /// Phase 39.B. One-time data migrations that have already run.
    #[serde(default)]
    pub migrations: MigrationFlags,
    /// Phase 58. Voice input (speech-to-text). Default backend is the
    /// browser-native Web Speech API; users with privacy / offline
    /// needs can point at a local Whisper-compatible endpoint.
    /// `#[serde(default)]` so older settings.json files load with
    /// `enabled = false` + the default backend.
    #[serde(default)]
    pub stt: SttSettings,
    /// Phase 62.B (item I): sidebar display mode — "full" | "icons" |
    /// "hidden". A String (not an enum) to match the rtl_mode /
    /// matcher_mode pattern and keep the TS binding a plain union.
    /// Persisted here (atomic settings write, Rule #7) so the choice
    /// survives restarts. `default = "full"` keeps older settings.json
    /// loading unchanged. Phase 65.P: only "full" / "icons" are written
    /// now; a legacy "hidden" value is migrated to "icons" on the
    /// frontend at read time (App.tsx sidebarMode()).
    #[serde(default = "default_sidebar_mode")]
    pub sidebar_mode: String,
    /// Phase 63: per-kind (browser / filemanager) floating-window state —
    /// which of the 3 modes each is in, plus the remembered geometry for
    /// Float / Pop-out / Pane. `#[serde(default)]` so older settings.json
    /// loads with both kinds defaulting to Float (current behavior).
    #[serde(default)]
    pub floating_windows: FloatingWindows,
    /// Phase 75: debug-log hygiene (retention). `#[serde(default)]` so older
    /// settings.json files load with the built-in defaults.
    #[serde(default)]
    pub logs: LogsSettings,
    /// Unshipped-fivefer (#3): keep workspace-browser cookies/logins across
    /// restarts. Backed by a single app-wide WebView2 profile folder (NOT a
    /// per-workspace `--user-data-dir` — that reintroduces the 0x8007139F
    /// crash). When false, the profile folder is wiped on the next launch.
    /// `default = true`.
    #[serde(default = "default_true")]
    pub persist_browser_sessions: bool,
    /// beta.3: which hook types the backend processes, and which of those
    /// play a sound on the toast. Kept in its own struct so the settings.rs
    /// `Hooks` block (policy engine / matcher_mode / auto_install) stays
    /// scoped to CLI-side hook installation, while this new struct is
    /// purely about desktop-side event routing + sound feedback.
    ///
    /// **Naming note:** the task brief called this field `hooks`, but that
    /// name is taken by the existing policy-engine struct. Named
    /// `hook_notifications` here to preserve backwards-compat without
    /// migrating the old field. `#[serde(default)]` fills defaults when a
    /// pre-beta.3 settings.json loads.
    #[serde(default)]
    pub hook_notifications: HookSettings,
}

fn default_sidebar_mode() -> String {
    "full".to_string()
}

/// Unshipped-fivefer (#3): read just the `persist_browser_sessions` flag as
/// early as possible in `run()` — before the WebView2 environment is created —
/// without needing app state. Missing file / parse error → default true.
pub(crate) fn persist_browser_sessions_flag() -> bool {
    load_from_disk()
        .map(|s| s.persist_browser_sessions)
        .unwrap_or(true)
}

/// Phase 75: debug.log retention. The log auto-rotates at a size cap and is
/// pruned on startup once older than `retention_days`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct LogsSettings {
    /// Delete debug logs untouched for this many days (0 = keep forever).
    #[serde(default = "default_log_retention_days")]
    pub retention_days: u32,
}

fn default_log_retention_days() -> u32 {
    7
}

impl Default for LogsSettings {
    fn default() -> Self {
        Self {
            retention_days: default_log_retention_days(),
        }
    }
}

/// Phase 63: display mode for a per-workspace Browser / File-Manager
/// window. `Pane` = docked to the side; `Float` = the modal-style window
/// over the workspace (the pre-63 behavior); `PopOut` = a standalone OS
/// window. Lowercased in JSON → TS union "pane" | "float" | "popout".
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum FloatingWindowMode {
    Pane,
    #[default]
    Float,
    PopOut,
}

/// Phase 63: a window rectangle in logical pixels.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Phase 63: persisted state for ONE floating-window kind, shared across
/// workspaces (the window is per-workspace, but its mode + geometry
/// preferences are global per kind — matches Yossi's spec).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct FloatingWindowState {
    #[serde(default)]
    pub mode: FloatingWindowMode,
    /// Last Float-mode rect (in-app, over the workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub float_rect: Option<Rect>,
    /// Last Pop-out OS-window rect (screen coordinates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popout_rect: Option<Rect>,
    /// Monitor index the Pop-out last lived on; if that monitor is gone
    /// next launch, fall back to the main window's display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popout_display: Option<i32>,
    /// Last Pane-mode width (px).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_width: Option<u32>,
}

/// Phase 63: both floating-window kinds.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct FloatingWindows {
    #[serde(default)]
    pub browser: FloatingWindowState,
    #[serde(default)]
    pub filemanager: FloatingWindowState,
}

/// Phase 58: speech-to-text settings.
///
/// - `Webspeech` uses `window.SpeechRecognition` directly in the
///   frontend (Chromium / WebView2 ships with it; Firefox does not —
///   not a concern for Tauri's WebView2-only Windows build, but worth
///   flagging if we ever target Linux's WebKitGTK).
/// - `Local` POSTs the recorded audio bytes to a user-configurable
///   HTTP endpoint (whisper.cpp's server, faster-whisper-server,
///   OpenAI-compatible local proxies). Field shape mirrors OpenAI's
///   /v1/audio/transcriptions: multipart with `file` (audio bytes) +
///   `language`.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct SttSettings {
    /// Master on/off. Default off — opt-in feature, no mic access
    /// requested until the user flips this.
    #[serde(default)]
    pub enabled: bool,
    /// Which backend to use when recording. Defaults to Webspeech.
    #[serde(default)]
    pub backend: SttBackend,
    /// Required when `backend = Local`. Skipped when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<String>,
    /// BCP-47 tag or "auto". Defaults to "auto" — the Web Speech API
    /// accepts it and most Whisper servers default to language
    /// detection when the param is missing or "auto".
    #[serde(default = "default_stt_lang")]
    pub language: String,
    /// Push-to-talk keybinding. Parsed by the existing shortcut-table
    /// helpers (Ctrl/Shift/Alt + key). Default Ctrl+Shift+M (M for
    /// microphone).
    #[serde(default = "default_stt_hotkey")]
    pub push_to_talk_hotkey: String,
}

/// Phase 58: backend choice. ts-rs lowercases via the serde attr so
/// the TS union is `"webspeech" | "local"`, matching the JSON.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum SttBackend {
    #[default]
    Webspeech,
    Local,
}

fn default_stt_lang() -> String {
    "auto".to_string()
}
fn default_stt_hotkey() -> String {
    "Ctrl+Shift+M".to_string()
}

/// Phase 39.B: tracks one-time data migrations so they run exactly
/// once. Each field is a "has-run" boolean defaulting to false.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct MigrationFlags {
    /// Phase 39: the auto_port_forward default flipped true→false. This
    /// migration flips all EXISTING workspaces' value to false to stop
    /// the post-connect auto-forward storm. Users re-enable per
    /// workspace; the flag keeps that choice from being undone.
    #[serde(default)]
    pub phase_39_auto_port_forward_default_flipped: bool,
    /// Phase 53 (rebased): the per-pane Browser / FileManager pane
    /// kinds were folded into workspace-level singleton windows. Any
    /// PaneKind::Browser or ::FileManager pane in a loaded
    /// workspaces.json is rewritten to PaneKind::Terminal on first
    /// load after upgrade. The flag stops the rewrite from running on
    /// every subsequent load (a Terminal pane that the user explicitly
    /// chose post-migration should NOT be touched).
    #[serde(default)]
    pub phase_53_remove_browser_filemanager_panes: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            preset: "tokyo-night".into(),
            accent: "#7aa2f7".into(),
            background: "#0e1116".into(),
            surface: "#161b22".into(),
            border: "#21262d".into(),
            text_primary: "#e6edf3".into(),
            text_secondary: "#7d8590".into(),
            success: "#4ec9b0".into(),
            warning: "#e0af68".into(),
            error: "#f7768e".into(),
            ansi: AnsiPalette::tokyo_night(),
        }
    }
}

impl Default for Font {
    fn default() -> Self {
        Self {
            ui_family: "system-ui".into(),
            ui_size_pt: 13,
            terminal_family: "Cascadia Mono".into(),
            terminal_size_pt: 13,
            web_font_url: None,
        }
    }
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            cursor_style: "bar".into(),
            scrollback_lines: 10000,
            bidi_enabled: true,
            allow_proposed_api: true,
            rtl_mode: default_rtl_mode(),
            use_winmux_tmux_config: true,
            mirror_arrows_rtl: true,
            auto_direction: true,
            auto_reset_on_connect: true,
        }
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: vec!["claude".into()],
            policy_preset: "default".into(),
            matcher_mode: default_matcher_mode(),
            policy_enabled: true,
            auto_install: true,
        }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            toast_enabled: true,
            sound_enabled: false,
            toast_session_start: false,
            // v0.4.4: SessionEnd ("session closed") is a rare, meaningful
            // signal — default it ON so the user actually learns a session
            // ended. (SessionStart stays OFF; that hook is no longer even
            // registered.)
            toast_session_end: true,
            toast_stop: true,
            toast_notification: true,
            toast_gate: true,
            toast_block: true,
            pane_pulse_on_activity: true,
        }
    }
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            auto_download: false,
            // Real manifest served as a static file from the repo's main
            // branch via raw.githubusercontent.com — no GitHub Pages, no
            // API rate limits. Updated as part of each release flow
            // (see RELEASING.md). A power user can override the URL
            // here without recompiling.
            manifest_url: Some(DEFAULT_MANIFEST_URL.into()),
            last_check_iso: None,
            last_seen_version: None,
            skipped_versions: Vec::new(),
            remind_after_iso: None,
            channel: default_channel(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::default(),
            font: Font::default(),
            terminal: TerminalSettings::default(),
            hooks: Hooks::default(),
            notifications: Notifications::default(),
            updates: Updates::default(),
            i18n: I18n::default(),
            shortcuts: Shortcuts::default(),
            claude: ClaudeOptions::default(),
            claude_usage: ClaudeUsageSettings::default(),
            hooks_updates: HooksUpdates::default(),
            ssh_key_offer_disabled: false,
            auto_connect_on_workspace_select: true,
            auto_destroy_empty_workspaces_days: None,
            migrations: MigrationFlags::default(),
            stt: SttSettings::default(),
            sidebar_mode: default_sidebar_mode(),
            floating_windows: FloatingWindows::default(),
            logs: LogsSettings::default(),
            persist_browser_sessions: true,
            hook_notifications: HookSettings::default(),
        }
    }
}

// ─── presets ───────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct PresetEntry {
    pub id: String,
    pub label: String,
    pub theme: Theme,
}

pub(crate) fn list_presets() -> Vec<PresetEntry> {
    vec![
        PresetEntry {
            id: "tokyo-night".into(),
            label: "Tokyo Night".into(),
            theme: Theme::default(),
        },
        PresetEntry {
            id: "dracula".into(),
            label: "Dracula".into(),
            theme: Theme {
                preset: "dracula".into(),
                accent: "#bd93f9".into(),
                background: "#282a36".into(),
                surface: "#21222c".into(),
                border: "#44475a".into(),
                text_primary: "#f8f8f2".into(),
                text_secondary: "#6272a4".into(),
                success: "#50fa7b".into(),
                warning: "#f1fa8c".into(),
                error: "#ff5555".into(),
                ansi: AnsiPalette::dracula(),
            },
        },
        PresetEntry {
            id: "solarized-dark".into(),
            label: "Solarized Dark".into(),
            theme: Theme {
                preset: "solarized-dark".into(),
                accent: "#268bd2".into(),
                background: "#002b36".into(),
                surface: "#073642".into(),
                border: "#586e75".into(),
                text_primary: "#eee8d5".into(),
                text_secondary: "#93a1a1".into(),
                success: "#859900".into(),
                warning: "#b58900".into(),
                error: "#dc322f".into(),
                ansi: AnsiPalette::solarized_dark(),
            },
        },
        PresetEntry {
            id: "nord".into(),
            label: "Nord".into(),
            theme: Theme {
                preset: "nord".into(),
                accent: "#88c0d0".into(),
                background: "#2e3440".into(),
                surface: "#3b4252".into(),
                border: "#4c566a".into(),
                text_primary: "#eceff4".into(),
                text_secondary: "#d8dee9".into(),
                success: "#a3be8c".into(),
                warning: "#ebcb8b".into(),
                error: "#bf616a".into(),
                ansi: AnsiPalette::nord(),
            },
        },
        PresetEntry {
            id: "solarized-light".into(),
            label: "Solarized Light".into(),
            theme: Theme {
                preset: "solarized-light".into(),
                accent: "#268bd2".into(),
                background: "#fdf6e3".into(),
                surface: "#eee8d5".into(),
                border: "#93a1a1".into(),
                text_primary: "#073642".into(),
                text_secondary: "#586e75".into(),
                success: "#859900".into(),
                warning: "#b58900".into(),
                error: "#dc322f".into(),
                ansi: AnsiPalette::solarized_light(),
            },
        },
    ]
}

pub(crate) fn get_preset(id: &str) -> Option<Theme> {
    list_presets().into_iter().find(|p| p.id == id).map(|p| p.theme)
}

// ─── disk I/O ──────────────────────────────────────────────────────────────

fn settings_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("settings.json"))
}

fn save_to_disk(file: &Settings) -> Result<(), String> {
    use std::io::Write as _;
    let path = settings_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("settings.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    dlog(&format!("settings save: {} bytes -> {:?}", text.len(), path));
    Ok(())
}

/// Public wrapper used by other modules (updater) that want to atomically
/// persist the current settings without going through `mutate` (e.g. they
/// already hold the lock).
pub(crate) fn save_to_disk_pub(file: &Settings) -> Result<(), String> {
    save_to_disk(file)
}

/// The canonical update manifest (raw.githubusercontent — no API rate limit).
const DEFAULT_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/yyhezkel/winmux/main/manifest.json";

/// One-shot fixups for an on-disk settings.json written by an older winmux.
/// Returns true if anything changed (so the caller re-persists). Phase 71:
/// an early default shipped a placeholder `winmux.example.com` manifest URL
/// that can never resolve — it caused the recurring `hooks-check: fetch
/// manifest failed` DNS spam. Replace any example/placeholder host with the
/// real default so update checks (and the version banner) work.
fn migrate_settings(s: &mut Settings) -> bool {
    let mut changed = false;
    let is_placeholder = s
        .updates
        .manifest_url
        .as_deref()
        .map(|u| {
            let l = u.to_ascii_lowercase();
            u.trim().is_empty()
                || l.contains("example.com")
                || l.contains("example.org")
                || l.contains("your-domain")
                || l.contains("changeme")
        })
        .unwrap_or(true);
    if is_placeholder {
        s.updates.manifest_url = Some(DEFAULT_MANIFEST_URL.to_string());
        changed = true;
        dlog("settings: migrated placeholder manifest_url → default");
    }
    changed
}

pub(crate) fn load_from_disk() -> Result<Settings, String> {
    let path = settings_path()?;
    if !path.exists() {
        let s = Settings::default();
        // Write the defaults so a power user can hand-edit without first
        // discovering it in the UI. Best-effort — don't fail load if the
        // initial write hits a permissions issue.
        if let Err(e) = save_to_disk(&s) {
            dlog(&format!("settings: initial save failed: {e}"));
        }
        return Ok(s);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {:?}: {e}", path))?;
    let parsed: Result<Settings, _> = serde_json::from_str(text.trim_start_matches('\u{FEFF}'));
    match parsed {
        Ok(mut s) => {
            if migrate_settings(&mut s) {
                // Persist the migrated values so the fix sticks (and the
                // placeholder-URL spam stops for good). Best-effort.
                let _ = save_to_disk(&s);
            }
            Ok(s)
        }
        Err(e) => {
            // Forward-compat: if the schema grew, fall back to defaults rather
            // than refusing to start. The user can re-save from the UI to
            // upgrade their on-disk file.
            dlog(&format!(
                "settings: parse {:?} failed ({e}) — using defaults",
                path
            ));
            Ok(Settings::default())
        }
    }
}

fn persist(state: &AppState) -> Result<(), String> {
    let s = state.settings.lock().unwrap().clone();
    save_to_disk(&s)
}

fn mutate<F: FnOnce(&mut Settings) -> Result<(), String>>(
    state: &AppState,
    app: &AppHandle,
    f: F,
) -> Result<Settings, String> {
    {
        let mut s = state.settings.lock().unwrap();
        f(&mut s)?;
    }
    persist(state)?;
    let s = state.settings.lock().unwrap().clone();
    let _ = app.emit("settings:changed", &s);
    Ok(s)
}

// ─── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn settings_load(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub(crate) fn settings_save(
    state: State<'_, AppState>,
    app: AppHandle,
    settings: Settings,
) -> Result<Settings, String> {
    mutate(&state, &app, |s| {
        *s = settings;
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn settings_get_presets() -> Vec<PresetEntry> {
    list_presets()
}

#[tauri::command]
pub(crate) fn settings_apply_preset(
    state: State<'_, AppState>,
    app: AppHandle,
    preset: String,
) -> Result<Settings, String> {
    let theme = get_preset(&preset).ok_or_else(|| format!("unknown preset {preset}"))?;
    mutate(&state, &app, |s| {
        s.theme = theme;
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn settings_reset(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Settings, String> {
    mutate(&state, &app, |s| {
        *s = Settings::default();
        Ok(())
    })
}

#[derive(Clone, Serialize)]
pub(crate) struct FontFamilies {
    pub ui: Vec<String>,
    pub mono: Vec<String>,
}

/// Best-effort enumeration of installed font families on Windows. Reads
/// `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts` and a handful
/// of mono-font hints. If anything fails (non-Windows, registry locked,
/// etc.) we fall back to a curated baseline so the picker is always
/// usable.
#[tauri::command]
pub(crate) fn list_system_fonts() -> FontFamilies {
    let baseline_ui = vec![
        "system-ui".to_string(),
        "Segoe UI Variable".to_string(),
        "Segoe UI".to_string(),
        "Inter".to_string(),
        "Roboto".to_string(),
        "Tahoma".to_string(),
        "Arial".to_string(),
    ];
    let baseline_mono = vec![
        "Cascadia Mono".to_string(),
        "Cascadia Code".to_string(),
        "JetBrains Mono".to_string(),
        "Consolas".to_string(),
        "Courier New".to_string(),
        "ui-monospace".to_string(),
        "monospace".to_string(),
    ];
    let mut all: Vec<String> = enumerate_windows_fonts().unwrap_or_default();
    all.sort();
    all.dedup();
    if all.is_empty() {
        return FontFamilies {
            ui: baseline_ui,
            mono: baseline_mono,
        };
    }
    let mono_hints = [
        "mono", "consolas", "cascadia", "courier", "menlo", "fira", "jetbrains",
        "iosevka", "hack", "source code", "lucida console",
    ];
    let mono: Vec<String> = all
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            mono_hints.iter().any(|h| lower.contains(h))
        })
        .cloned()
        .collect();
    let ui: Vec<String> = all
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            !mono_hints.iter().any(|h| lower.contains(h))
                && !lower.contains("symbol")
                && !lower.contains("emoji")
                && !lower.contains("wingdings")
        })
        .cloned()
        .collect();
    let merge = |mut head: Vec<String>, tail: Vec<String>| -> Vec<String> {
        for t in tail {
            if !head.iter().any(|h| h.eq_ignore_ascii_case(&t)) {
                head.push(t);
            }
        }
        head
    };
    FontFamilies {
        ui: merge(baseline_ui, ui),
        mono: merge(baseline_mono, mono),
    }
}

#[cfg(target_os = "windows")]
fn enumerate_windows_fonts() -> Option<Vec<String>> {
    // Spawn a tiny PowerShell call rather than pulling in winreg as a dep.
    // Output is one font name per line. Best-effort: errors → None.
    use std::process::Command;
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts' | \
             Get-Member -MemberType NoteProperty | Where-Object { $_.Name -notmatch '^PS' } | \
             ForEach-Object { ($_.Name -replace ' \\(TrueType\\)$','') -replace ' \\(OpenType\\)$','' }",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut families: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        // Drop legacy .FON bitmap-font registry entries such as
        // "Courier 10,12,15 (VGA res)" / "MS Sans Serif 8,10,12,14,18,24 (120)".
        // A real CSS font-family name never contains a comma, and these
        // bitmap fonts can't render in the webview anyway; leaking them let
        // the picker store an unusable name whose internal commas then
        // fragmented the CSS font-family list (compressed panes).
        .filter(|l| !l.contains(','))
        .collect();
    // Strip variant suffixes like "Bold", "Italic" so the picker shows
    // family names, not every weight.
    for name in families.iter_mut() {
        for suffix in [" Bold Italic", " Bold", " Italic", " Light", " Black", " Semibold"] {
            if let Some(stripped) = name.strip_suffix(suffix) {
                *name = stripped.to_string();
            }
        }
    }
    Some(families)
}

#[cfg(not(target_os = "windows"))]
fn enumerate_windows_fonts() -> Option<Vec<String>> {
    None
}

// ─── helpers exposed to RPC dispatch ───────────────────────────────────────

/// Apply a partial JSON patch (object) on top of the current settings,
/// merging recursively. Fields absent from the patch are preserved.
pub(crate) fn rpc_patch(
    state: &AppState,
    app: &AppHandle,
    patch: Value,
) -> Result<Settings, String> {
    mutate(state, app, |s| {
        let mut as_value = serde_json::to_value(&*s).map_err(|e| e.to_string())?;
        merge_in_place(&mut as_value, &patch);
        let next: Settings =
            serde_json::from_value(as_value).map_err(|e| format!("merged settings invalid: {e}"))?;
        *s = next;
        Ok(())
    })
}

/// Apply a single dotted-path setting (e.g. `theme.preset = "dracula"`).
/// Strings, numbers, and booleans are accepted; everything else falls back
/// to JSON parsing of the string value.
pub(crate) fn rpc_set_path(
    state: &AppState,
    app: &AppHandle,
    path: &str,
    value: &str,
) -> Result<Settings, String> {
    let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
    let mut patch = Value::Object(Default::default());
    insert_at_path(&mut patch, path, parsed)?;
    rpc_patch(state, app, patch)
}

pub(crate) fn rpc_apply_preset(
    state: &AppState,
    app: &AppHandle,
    preset: &str,
) -> Result<Settings, String> {
    let theme = get_preset(preset).ok_or_else(|| format!("unknown preset {preset}"))?;
    mutate(state, app, |s| {
        s.theme = theme;
        Ok(())
    })
}

fn merge_in_place(into: &mut Value, from: &Value) {
    if let (Value::Object(a), Value::Object(b)) = (&mut *into, from) {
        for (k, v) in b {
            match a.get_mut(k) {
                Some(existing) if existing.is_object() && v.is_object() => {
                    merge_in_place(existing, v);
                }
                _ => {
                    a.insert(k.clone(), v.clone());
                }
            }
        }
    } else {
        *into = from.clone();
    }
}

fn insert_at_path(root: &mut Value, path: &str, leaf: Value) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("empty path".into());
    }
    let mut cur = root;
    for (i, p) in parts.iter().enumerate() {
        if !cur.is_object() {
            *cur = Value::Object(Default::default());
        }
        let obj = cur.as_object_mut().unwrap();
        if i == parts.len() - 1 {
            obj.insert((*p).into(), leaf.clone());
            return Ok(());
        }
        cur = obj
            .entry((*p).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{migrate_settings, HookType, MigrationFlags, Settings, DEFAULT_MANIFEST_URL};

    #[test]
    fn migrates_placeholder_manifest_url() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("https://winmux.example.com/manifest.json".into());
        assert!(migrate_settings(&mut s), "should report a change");
        assert_eq!(s.updates.manifest_url.as_deref(), Some(DEFAULT_MANIFEST_URL));
    }

    #[test]
    fn migrates_empty_manifest_url() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("".into());
        assert!(migrate_settings(&mut s));
        assert_eq!(s.updates.manifest_url.as_deref(), Some(DEFAULT_MANIFEST_URL));
    }

    #[test]
    fn leaves_real_manifest_url_alone() {
        let mut s = Settings::default();
        s.updates.manifest_url = Some("https://example-server.io/my.json".into());
        // "example-server.io" is NOT a placeholder host (no "example.com").
        assert!(!migrate_settings(&mut s));
        assert_eq!(s.updates.manifest_url.as_deref(), Some("https://example-server.io/my.json"));
    }

    #[test]
    fn migration_flags_default_is_not_run() {
        let f = MigrationFlags::default();
        assert!(!f.phase_39_auto_port_forward_default_flipped);
        // Settings default carries an un-run MigrationFlags.
        assert_eq!(Settings::default().migrations, f);
    }

    #[test]
    fn migration_flag_round_trips() {
        let mut s = Settings::default();
        s.migrations.phase_39_auto_port_forward_default_flipped = true;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.migrations.phase_39_auto_port_forward_default_flipped);
    }

    #[test]
    fn pre_39b_settings_json_defaults_flag_false() {
        // A settings.json written before 39.B has no `migrations` key.
        let json = r##"{"version":1,"theme":{"preset":"x","accent":"#000","background":"#000","surface":"#000","border":"#000","text_primary":"#000","text_secondary":"#000","success":"#000","warning":"#000","error":"#000","ansi":{"black":"#000","red":"#000","green":"#000","yellow":"#000","blue":"#000","magenta":"#000","cyan":"#000","white":"#000","bright_black":"#000","bright_red":"#000","bright_green":"#000","bright_yellow":"#000","bright_blue":"#000","bright_magenta":"#000","bright_cyan":"#000","bright_white":"#000"}},"font":{"ui_family":"x","ui_size_pt":13,"terminal_family":"x","terminal_size_pt":13},"terminal":{"cursor_style":"bar","scrollback_lines":1000,"bidi_enabled":true,"allow_proposed_api":true},"hooks":{"enabled":true,"agents":[],"policy_preset":"default"},"notifications":{"toast_enabled":true,"sound_enabled":false},"updates":{"check_on_startup":true,"auto_download":false}}"##;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.migrations.phase_39_auto_port_forward_default_flipped);
        // Phase 41: the same pre-41 JSON has no auto_connect field either —
        // serde(default = "default_true") must fill it in as true.
        assert!(
            s.auto_connect_on_workspace_select,
            "missing auto_connect_on_workspace_select must default to true"
        );
    }

    #[test]
    fn auto_connect_default_is_true_and_round_trips() {
        assert!(Settings::default().auto_connect_on_workspace_select);
        let mut s = Settings::default();
        s.auto_connect_on_workspace_select = false;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(!back.auto_connect_on_workspace_select);
    }

    #[test]
    fn beta3_hook_settings_default_is_interactive_four() {
        let s = Settings::default().hook_notifications;
        // Interactive-4 enabled.
        assert!(s.enabled_types.contains(&HookType::PreToolUse));
        assert!(s.enabled_types.contains(&HookType::Notification));
        assert!(s.enabled_types.contains(&HookType::Stop));
        assert!(s.enabled_types.contains(&HookType::SessionEnd));
        // Observability off by default.
        assert!(!s.enabled_types.contains(&HookType::PostToolUse));
        assert!(!s.enabled_types.contains(&HookType::SubagentStop));
        assert!(!s.enabled_types.contains(&HookType::UserPromptSubmit));
        assert!(!s.enabled_types.contains(&HookType::PreCompact));
        assert!(!s.enabled_types.contains(&HookType::SessionStart));
        // Interactive-3 sound-on.
        assert!(s.sound_types.contains(&HookType::PreToolUse));
        assert!(s.sound_types.contains(&HookType::Notification));
        assert!(s.sound_types.contains(&HookType::Stop));
        // SessionEnd enabled but silent by default.
        assert!(!s.sound_types.contains(&HookType::SessionEnd));
        assert!(s.sound_master);
    }

    #[test]
    fn beta3_pre_hook_notifications_settings_json_populates_defaults() {
        // A settings.json written before beta.3 has no `hook_notifications`
        // key. The migration must fill it with the interactive-4 default
        // (see `default_hook_notifications`).
        let json = r##"{"version":1,"theme":{"preset":"x","accent":"#000","background":"#000","surface":"#000","border":"#000","text_primary":"#000","text_secondary":"#000","success":"#000","warning":"#000","error":"#000","ansi":{"black":"#000","red":"#000","green":"#000","yellow":"#000","blue":"#000","magenta":"#000","cyan":"#000","white":"#000","bright_black":"#000","bright_red":"#000","bright_green":"#000","bright_yellow":"#000","bright_blue":"#000","bright_magenta":"#000","bright_cyan":"#000","bright_white":"#000"}},"font":{"ui_family":"x","ui_size_pt":13,"terminal_family":"x","terminal_size_pt":13},"terminal":{"cursor_style":"bar","scrollback_lines":1000,"bidi_enabled":true,"allow_proposed_api":true},"hooks":{"enabled":true,"agents":[],"policy_preset":"default"},"notifications":{"toast_enabled":true,"sound_enabled":false},"updates":{"check_on_startup":true,"auto_download":false}}"##;
        let s: Settings = serde_json::from_str(json).unwrap();
        // Serde default kicked in — sound_master + interactive-4 enabled.
        assert!(s.hook_notifications.sound_master);
        assert!(s.hook_notifications.enabled_types.contains(&HookType::Stop));
        assert!(!s
            .hook_notifications
            .enabled_types
            .contains(&HookType::PostToolUse));
    }

    #[test]
    fn beta3_hook_type_wire_form_is_kebab_case() {
        // The enum serializes back to the same kebab-case strings the CLI
        // emits (see rpc_server.rs subkind dispatch). Locked here so a
        // rename can't silently break the wire protocol.
        assert_eq!(
            serde_json::to_string(&HookType::PreToolUse).unwrap(),
            "\"pre-tool-use\""
        );
        assert_eq!(
            serde_json::to_string(&HookType::SessionEnd).unwrap(),
            "\"session-end\""
        );
        assert_eq!(
            serde_json::to_string(&HookType::UserPromptSubmit).unwrap(),
            "\"user-prompt-submit\""
        );
    }
}
