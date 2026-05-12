//! Phase 12.C: local PTY mini-wizard.
//!
//! Two affordances surfaced in `CreateWorkspaceModal` when Type=Local:
//!
//! 1. `detect_local_shells()` — surface what's actually installed
//!    (PowerShell 7, Windows PowerShell, cmd, Git Bash, WSL) so the
//!    user can pick by label instead of typing a binary path.
//!
//! 2. `recent_paths` — a small JSON store of recently-used cwds for
//!    local PTY workspaces. The cwd combobox is seeded with built-in
//!    defaults ($USERPROFILE, ~/Documents, ~/source) PLUS the user's
//!    own history sorted by recency. We record one entry per
//!    `record_recent_path()` call, dedupe, cap at 20 entries.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{config_dir_pub, dlog, AppState};

// ─── shell detection ──────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct ShellInfo {
    /// Stable id used by the frontend to look up the i18n label.
    pub id: String,
    /// Human-readable label as a fallback (English).
    pub label: String,
    /// The actual command to spawn (path, with arguments if needed).
    pub command: String,
    pub available: bool,
}

#[cfg(target_os = "windows")]
fn which(exe: &str) -> Option<PathBuf> {
    // Cheap PATH lookup — no need for the `which` crate.
    let path_env = std::env::var("PATH").ok()?;
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
    let exts: Vec<&str> = pathext.split(';').collect();
    for dir in path_env.split(';') {
        if dir.is_empty() {
            continue;
        }
        let base = PathBuf::from(dir);
        // If the input already has a recognized extension, try as-is.
        if exts.iter().any(|e| exe.to_lowercase().ends_with(&e.to_lowercase())) {
            let p = base.join(exe);
            if p.is_file() {
                return Some(p);
            }
        }
        for ext in &exts {
            let p = base.join(format!("{exe}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn which(_exe: &str) -> Option<PathBuf> {
    None
}

fn pwsh_canonical() -> Option<PathBuf> {
    // Prefer the canonical Program Files install over a sideloaded one
    // on PATH so a confused user doesn't get a stale 7.0 from
    // %USERPROFILE%\AppData. Falls back to PATH if not present.
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let canonical = PathBuf::from(pf).join("PowerShell\\7\\pwsh.exe");
    if canonical.is_file() {
        return Some(canonical);
    }
    which("pwsh.exe")
}

fn git_bash() -> Option<PathBuf> {
    // Git for Windows ships bash at `<install>\bin\bash.exe`, and the
    // graphical Git Bash launcher at `<install>\git-bash.exe`. We
    // prefer the launcher (it sets up the terminal env), but fall back
    // to bash.exe if the launcher isn't there.
    for env_var in ["ProgramFiles", "ProgramFiles(x86)"] {
        let pf = std::env::var(env_var).ok()?;
        let candidates = [
            PathBuf::from(&pf).join("Git\\bin\\bash.exe"),
            PathBuf::from(&pf).join("Git\\git-bash.exe"),
        ];
        for c in candidates {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    which("git-bash.exe").or_else(|| which("bash.exe"))
}

fn wsl_available() -> bool {
    // `wsl --status` exits 0 when there's at least one distro registered.
    // `which` alone can succeed on machines where the Store stub is
    // installed but there's no distro — we want the real thing.
    match which("wsl.exe") {
        Some(_) => {}
        None => return false,
    }
    let out = std::process::Command::new("wsl")
        .args(["--status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match out {
        Ok(o) => o.status.success() && !o.stdout.is_empty(),
        Err(_) => false,
    }
}

#[tauri::command]
pub(crate) fn detect_local_shells() -> Vec<ShellInfo> {
    let mut out = Vec::new();

    if let Some(p) = pwsh_canonical() {
        out.push(ShellInfo {
            id: "pwsh".into(),
            label: "PowerShell 7".into(),
            command: p.to_string_lossy().to_string(),
            available: true,
        });
    } else {
        out.push(ShellInfo {
            id: "pwsh".into(),
            label: "PowerShell 7".into(),
            command: "pwsh.exe".into(),
            available: false,
        });
    }

    // Windows PowerShell ships with the OS — assume present.
    out.push(ShellInfo {
        id: "powershell".into(),
        label: "Windows PowerShell".into(),
        command: which("powershell.exe")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "powershell.exe".into()),
        available: true,
    });

    out.push(ShellInfo {
        id: "cmd".into(),
        label: "Command Prompt".into(),
        command: "cmd.exe".into(),
        available: true,
    });

    if let Some(p) = git_bash() {
        out.push(ShellInfo {
            id: "gitbash".into(),
            label: "Git Bash".into(),
            command: p.to_string_lossy().to_string(),
            available: true,
        });
    }

    if wsl_available() {
        out.push(ShellInfo {
            id: "wsl".into(),
            label: "WSL bash".into(),
            command: "wsl.exe bash -l".into(),
            available: true,
        });
    }

    out
}

// ─── recent paths ─────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct RecentPathEntry {
    pub path: String,
    /// Unix seconds.
    pub last_used: i64,
    /// Hit count — used for the frecency sort.
    pub uses: u32,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct RecentPathsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<RecentPathEntry>,
}

fn default_version() -> u32 {
    1
}

const MAX_RECENT: usize = 20;

fn recent_paths_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("recent_paths.json"))
}

pub(crate) fn load_recent_from_disk() -> Result<RecentPathsFile, String> {
    let path = recent_paths_path()?;
    if !path.exists() {
        return Ok(RecentPathsFile::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse {path:?}: {e}"))
}

fn save_recent_to_disk(file: &RecentPathsFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = recent_paths_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("recent_paths.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {tmp:?}: {e}"))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Defaults seeded into the combobox when there's no history yet (or
/// to fill out the bottom of a short list). Order = preference.
fn builtin_defaults() -> Vec<String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let mut v = Vec::new();
    if let Some(h) = home {
        v.push(h.clone());
        v.push(format!("{h}\\Documents"));
        v.push(format!("{h}\\source"));
        v.push(format!("{h}\\source\\repos"));
        v.push(format!("{h}\\Downloads"));
    }
    v
}

#[derive(Clone, Serialize)]
pub(crate) struct RecentPathSuggestion {
    pub path: String,
    pub kind: &'static str, // "recent" or "default"
}

/// Returns the recent paths sorted by frecency PLUS the built-in
/// defaults that aren't already in the recent list. Frontend renders
/// the two groups separately.
#[tauri::command]
pub(crate) fn list_recent_paths(state: State<'_, AppState>) -> Vec<RecentPathSuggestion> {
    let file = state.recent_paths.lock().unwrap().clone();
    let mut recents = file.entries.clone();
    // Frecency: weight uses by recency. Simpler than a full Mozilla-style
    // formula but works well for ≤20 entries — boost by uses, bias
    // towards last_used.
    recents.sort_by(|a, b| {
        let score_a = a.uses as i64 * 100 + a.last_used / 60;
        let score_b = b.uses as i64 * 100 + b.last_used / 60;
        score_b.cmp(&score_a)
    });
    let mut out: Vec<RecentPathSuggestion> = recents
        .iter()
        .map(|e| RecentPathSuggestion {
            path: e.path.clone(),
            kind: "recent",
        })
        .collect();
    let have: std::collections::HashSet<String> =
        out.iter().map(|e| e.path.to_lowercase()).collect();
    for d in builtin_defaults() {
        if !have.contains(&d.to_lowercase()) {
            out.push(RecentPathSuggestion {
                path: d,
                kind: "default",
            });
        }
    }
    out
}

/// Bump (or insert) a path in the recent list. Called by App.tsx when a
/// local PTY workspace connects.
#[tauri::command]
pub(crate) fn record_recent_path(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let cleaned = path.trim().to_string();
    if cleaned.is_empty() {
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    {
        let mut file = state.recent_paths.lock().unwrap();
        let cmp = cleaned.to_lowercase();
        if let Some(existing) = file
            .entries
            .iter_mut()
            .find(|e| e.path.to_lowercase() == cmp)
        {
            existing.uses += 1;
            existing.last_used = now;
        } else {
            file.entries.push(RecentPathEntry {
                path: cleaned,
                last_used: now,
                uses: 1,
            });
        }
        // Trim oldest by last_used if we go over cap.
        if file.entries.len() > MAX_RECENT {
            file.entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
            file.entries.truncate(MAX_RECENT);
        }
    }
    let snapshot = state.recent_paths.lock().unwrap().clone();
    if let Err(e) = save_recent_to_disk(&snapshot) {
        dlog(&format!("recent_paths save failed: {e}"));
    }
    Ok(())
}
