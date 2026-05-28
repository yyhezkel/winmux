//! Phase 9.A: app settings (theme + font + terminal + hooks + notifications +
//! updates). Persisted in `%APPDATA%\winmux\settings.json` next to
//! `workspaces.json` / `notes.json`. Same atomic-write + load-poison-gate
//! pattern. Mutations emit `settings:changed` to the frontend so live theme
//! updates from the CLI reflect into the UI without a reload.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, dlog, AppState};

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
}

fn default_matcher_mode() -> String {
    "restrictive".to_string()
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct Notifications {
    pub toast_enabled: bool,
    pub sound_enabled: bool,
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
    /// Phase 18. Hooks-outdated banner show/skip state.
    #[serde(default)]
    pub hooks_updates: HooksUpdates,
    /// Phase 32.B. When true, suppress the "set up SSH key
    /// authentication?" offer after a password-auth connect. Persisted
    /// when the user ticks "Don't show again" in the offer modal.
    /// `#[serde(default)]` so older settings.json loads cleanly.
    #[serde(default)]
    pub ssh_key_offer_disabled: bool,
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
        }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            toast_enabled: true,
            sound_enabled: false,
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
            manifest_url: Some(
                "https://raw.githubusercontent.com/yyhezkel/winmux/main/manifest.json".into(),
            ),
            last_check_iso: None,
            last_seen_version: None,
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
            hooks_updates: HooksUpdates::default(),
            ssh_key_offer_disabled: false,
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
        Ok(s) => Ok(s),
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
