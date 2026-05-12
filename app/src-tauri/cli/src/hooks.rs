//! `winmux setup-hooks` — register agent hooks that point at the local winmux CLI
//! so AI coding agents (Claude Code etc.) can pipe permission requests / lifecycle
//! events back through the tunnel into the Windows app's UI.
//!
//! Phase 18: the hook spec used to be a hardcoded `&[(event, subcmd, matcher)]`
//! slice. It now ships as a JSON file at the repo root (`hooks/<agent>.json`)
//! that the CLI fetches from raw.githubusercontent.com at install time, with
//! a `~/.winmux/cache/hooks/` fallback and the bundled spec as a final
//! last resort. The settings.json is annotated with `winmux_hooks_version` so
//! the desktop's outdated-check (also in Phase 18) can flag installs whose
//! hook spec is older than the latest published one.
//!
//! Designed to be idempotent and additive: existing entries unrelated to winmux are
//! preserved, and our entries are detected by a `winmux ... claude-hook` substring
//! match so we replace ourselves instead of accumulating duplicates.

use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

pub enum AgentStatus {
    NotDetected,
    Stub(String),
    Done {
        registered: Vec<String>,
        backup: Option<PathBuf>,
        path: PathBuf,
        unchanged: bool,
        /// Phase 18: the `winmux_hooks_version` that just landed in
        /// settings.json — used by the calling print code so the user
        /// sees which version we installed.
        hooks_version: Option<String>,
    },
    DryRun {
        would_register: Vec<String>,
        path: PathBuf,
        already_present: Vec<String>,
    },
    Error(String),
}

pub trait HookAdapter {
    fn label(&self) -> &'static str;
    fn run(&self, dry: bool, force: bool, source: &str) -> AgentStatus;
}

// ─── Hook spec (the shape of hooks/<agent>.json) ───────────────────────────

/// Parsed spec for one agent. Matches the JSON in `hooks/<agent>.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct HookSpec {
    pub winmux_hooks_version: String,
    #[allow(dead_code)]
    pub agent: String,
    #[serde(default)]
    pub events: std::collections::BTreeMap<String, HookEvent>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HookEvent {
    pub matcher: String,
    pub command: String,
}

/// The version this CLI was built with, used as the spec returned by
/// `source=bundled` AND as the version recorded when no fetched spec
/// was applied. Bump whenever you ship a new hook in a release with a
/// matching `hooks/claude-code.json` change.
const BUNDLED_CLAUDE_VERSION: &str = "1.0.0";

/// The bundled fallback spec for Claude Code. Mirrors what
/// `hooks/claude-code.json` carries at the same `winmux_hooks_version`
/// — kept in sync by hand for now (a `build.rs` that bakes the file
/// into the binary is on the roadmap).
fn bundled_claude_spec() -> HookSpec {
    use std::collections::BTreeMap;
    let mut events: BTreeMap<String, HookEvent> = BTreeMap::new();
    events.insert(
        "PreToolUse".into(),
        HookEvent {
            matcher: "Bash|Write|Edit|MultiEdit|NotebookEdit|Task".into(),
            command: "${WINMUX_BIN} claude-hook pre-tool-use".into(),
        },
    );
    for (ev, sub) in [
        ("Notification", "notification"),
        ("SessionStart", "session-start"),
        ("SessionEnd", "session-end"),
        ("Stop", "stop"),
    ] {
        events.insert(
            ev.into(),
            HookEvent {
                matcher: "*".into(),
                command: format!("${{WINMUX_BIN}} claude-hook {sub}"),
            },
        );
    }
    HookSpec {
        winmux_hooks_version: BUNDLED_CLAUDE_VERSION.into(),
        agent: "claude-code".into(),
        events,
    }
}

/// Resolve the spec per `--source`:
///   - `github`: fetch `raw.githubusercontent.com/yyhezkel/winmux/main/hooks/<agent>.json`,
///     fall through to cache, then to bundled.
///   - `bundled`: skip the network entirely.
///   - `url=<u>`: fetch from a custom URL, no cache fallback (the user
///     is opting into a specific source — silently swapping to bundled
///     would surprise them).
/// On every successful fetch, write the JSON to
/// `~/.winmux/cache/hooks/<agent>.json` so the next call without
/// network connectivity still picks up the latest spec.
pub fn load_spec(source: &str, agent_id: &str) -> Result<HookSpec, String> {
    let canonical_url = format!(
        "https://raw.githubusercontent.com/yyhezkel/winmux/main/hooks/{agent_id}.json"
    );

    match source {
        "bundled" => Ok(bundled_spec_for(agent_id)),
        "github" => {
            match fetch_url(&canonical_url) {
                Ok(text) => {
                    let spec = parse_spec(&text)?;
                    let _ = write_cache(agent_id, &text);
                    Ok(spec)
                }
                Err(e_fetch) => {
                    eprintln!("setup-hooks: github fetch failed ({e_fetch}) — trying cache");
                    if let Ok(text) = read_cache(agent_id) {
                        if let Ok(spec) = parse_spec(&text) {
                            eprintln!("setup-hooks: using cached spec");
                            return Ok(spec);
                        }
                    }
                    eprintln!("setup-hooks: cache miss — using bundled spec");
                    Ok(bundled_spec_for(agent_id))
                }
            }
        }
        s if s.starts_with("url=") => {
            let u = &s[4..];
            let text = fetch_url(u)?;
            let spec = parse_spec(&text)?;
            let _ = write_cache(agent_id, &text);
            Ok(spec)
        }
        other => Err(format!(
            "unknown --source {other:?} (expected github / bundled / url=<U>)"
        )),
    }
}

fn parse_spec(text: &str) -> Result<HookSpec, String> {
    serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse hook spec: {e}"))
}

fn bundled_spec_for(agent_id: &str) -> HookSpec {
    match agent_id {
        "claude-code" | "claude" => bundled_claude_spec(),
        // Other agents have no bundled fallback — return an empty
        // spec so the caller's apply step is a no-op. The github
        // path is the only way they get useful hooks today.
        other => HookSpec {
            winmux_hooks_version: "0.0.0".into(),
            agent: other.into(),
            events: Default::default(),
        },
    }
}

/// Shell out to curl / wget for the fetch. Both are universally
/// present on the Linux servers we target, and on Windows 10+ curl
/// ships in the base OS. We deliberately avoid pulling in `reqwest`
/// (the dep tree adds ~2 MB to the CLI binary for one HTTP GET).
fn fetch_url(url: &str) -> Result<String, String> {
    let curl = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "10", url])
        .output();
    match curl {
        Ok(o) if o.status.success() => {
            return Ok(String::from_utf8_lossy(&o.stdout).to_string());
        }
        Ok(o) => {
            // curl ran but returned non-zero. Try wget too before giving up.
            let curl_err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if let Some(out) = try_wget(url) {
                return Ok(out);
            }
            return Err(format!("curl exit {}: {curl_err}", o.status));
        }
        Err(_) => {
            if let Some(out) = try_wget(url) {
                return Ok(out);
            }
        }
    }
    Err("neither curl nor wget is available".into())
}

fn try_wget(url: &str) -> Option<String> {
    let out = std::process::Command::new("wget")
        .args(["-q", "-O", "-", "--timeout=10", url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn cache_path(agent_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".winmux")
            .join("cache")
            .join("hooks")
            .join(format!("{agent_id}.json")),
    )
}

fn write_cache(agent_id: &str, text: &str) -> Result<(), String> {
    let path = cache_path(agent_id).ok_or_else(|| "no $HOME".to_string())?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, text).map_err(|e| format!("cache write {path:?}: {e}"))
}

fn read_cache(agent_id: &str) -> Result<String, String> {
    let path = cache_path(agent_id).ok_or_else(|| "no $HOME".to_string())?;
    fs::read_to_string(&path).map_err(|e| format!("cache read {path:?}: {e}"))
}

// ─── Claude adapter ────────────────────────────────────────────────────────

pub struct Claude;

impl HookAdapter for Claude {
    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn run(&self, dry: bool, force: bool, source: &str) -> AgentStatus {
        let home = match std::env::var_os("HOME") {
            Some(h) => h,
            None => return AgentStatus::Error("$HOME not set".into()),
        };
        let claude_dir = PathBuf::from(&home).join(".claude");
        if !claude_dir.is_dir() {
            return AgentStatus::NotDetected;
        }

        let exe_path = match home.to_str() {
            Some(s) => format!("{}/.winmux/bin/winmux", s),
            None => return AgentStatus::Error("non-UTF-8 $HOME".into()),
        };

        let spec = match load_spec(source, "claude-code") {
            Ok(s) => s,
            Err(e) => return AgentStatus::Error(format!("load spec: {e}")),
        };

        // Phase setup-hooks-fix: current Claude Code reads hooks from
        // `~/.claude/settings.json` under a top-level `"hooks"` key — NOT
        // from a separate `~/.claude/hooks.json`. We write BOTH so:
        //   • settings.json is what Claude Code actually consumes,
        //   • hooks.json stays for any legacy tooling that might still read it.
        let settings_path = claude_dir.join("settings.json");
        let legacy_path = claude_dir.join("hooks.json");

        let settings_outcome =
            apply_to_settings(&claude_dir, &settings_path, &exe_path, &spec, dry, force);
        let _ = apply_to_legacy(&claude_dir, &legacy_path, &exe_path, &spec, dry, force);
        settings_outcome
    }
}

/// Substitute `${WINMUX_BIN}` (and the legacy bare `winmux`) in a
/// spec command string with the absolute path we just computed.
fn expand_command(cmd: &str, exe_path: &str) -> String {
    cmd.replace("${WINMUX_BIN}", exe_path)
}

fn apply_to_settings(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
    spec: &HookSpec,
    dry: bool,
    force: bool,
) -> AgentStatus {
    let mut root: Value = match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(text.trim_start_matches('\u{FEFF}')) {
            Ok(v) => v,
            Err(e) => return AgentStatus::Error(format!("parse {}: {}", path.display(), e)),
        },
        Err(_) => json!({}),
    };
    if !root.is_object() {
        return AgentStatus::Error(format!(
            "{} is not a JSON object (got {:?}); refusing to overwrite",
            path.display(),
            kind_of(&root)
        ));
    }
    if !root.get("hooks").is_some_and(|h| h.is_object()) {
        root["hooks"] = json!({});
    }

    let mut would_register: Vec<String> = Vec::new();
    let mut already_present: Vec<String> = Vec::new();
    let mut to_apply: Vec<(String, String, String)> = Vec::new();

    for (event, ev_spec) in &spec.events {
        let cmd = expand_command(&ev_spec.command, exe_path);
        let entries = root["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_winmux = entries.iter().any(is_winmux_entry);
        if has_winmux && !force {
            already_present.push(event.clone());
            continue;
        }
        would_register.push(event.clone());
        to_apply.push((event.clone(), cmd, ev_spec.matcher.clone()));
    }

    if dry {
        return AgentStatus::DryRun {
            would_register,
            path: path.to_path_buf(),
            already_present,
        };
    }

    if to_apply.is_empty() {
        // Even when no event entries change, refresh the meta tag so
        // the desktop's outdated-check picks up a version bump that's
        // purely a no-op (e.g. a spec rebuild with identical events).
        if root["winmux_meta"]
            .get("hooks_version")
            .and_then(|v| v.as_str())
            != Some(spec.winmux_hooks_version.as_str())
        {
            root["winmux_meta"] = json!({
                "hooks_version": spec.winmux_hooks_version,
                "agent": spec.agent,
            });
            let text = serde_json::to_string_pretty(&root).unwrap_or_default();
            let _ = fs::write(path, text);
        }
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
            hooks_version: Some(spec.winmux_hooks_version.clone()),
        };
    }

    let backup = if path.exists() {
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();
        let bk = claude_dir.join(format!("settings.json.bak.{}", stamp));
        if let Err(e) = fs::copy(path, &bk) {
            return AgentStatus::Error(format!("backup {}: {}", bk.display(), e));
        }
        Some(bk)
    } else {
        None
    };

    for (event, cmd, matcher) in &to_apply {
        let mut entries = root["hooks"][event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_winmux_entry(e));
        entries.push(json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": cmd }]
        }));
        root["hooks"][event.as_str()] = json!(entries);
    }

    // Phase 18: stamp the version into settings.json so the desktop's
    // outdated check has somewhere to read it back from. Lives under
    // a sibling `winmux_meta` key so we don't risk colliding with
    // anything Claude Code itself adds to its config.
    root["winmux_meta"] = json!({
        "hooks_version": spec.winmux_hooks_version,
        "agent": spec.agent,
    });

    let tmp = claude_dir.join(format!("settings.json.winmux-tmp.{}", std::process::id()));
    let text = match serde_json::to_string_pretty(&root) {
        Ok(t) => t,
        Err(e) => return AgentStatus::Error(format!("serialize: {e}")),
    };
    if let Err(e) = fs::write(&tmp, &text) {
        return AgentStatus::Error(format!("write tmp: {e}"));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return AgentStatus::Error(format!("rename: {e}"));
    }

    AgentStatus::Done {
        registered: to_apply.into_iter().map(|(e, _, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
        hooks_version: Some(spec.winmux_hooks_version.clone()),
    }
}

fn apply_to_legacy(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
    spec: &HookSpec,
    dry: bool,
    force: bool,
) -> AgentStatus {
    let mut config: Value = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{FEFF}')).unwrap_or(json!({})),
        Err(_) => json!({}),
    };
    if !config.is_object() {
        config = json!({});
    }

    let mut to_apply: Vec<(String, String, String)> = Vec::new();
    for (event, ev_spec) in &spec.events {
        let cmd = expand_command(&ev_spec.command, exe_path);
        let entries = config[event].as_array().cloned().unwrap_or_default();
        let has_winmux = entries.iter().any(is_winmux_entry);
        if has_winmux && !force {
            continue;
        }
        to_apply.push((event.clone(), cmd, ev_spec.matcher.clone()));
    }
    if dry || to_apply.is_empty() {
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
            hooks_version: Some(spec.winmux_hooks_version.clone()),
        };
    }

    let backup = if path.exists() {
        let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();
        let bk = claude_dir.join(format!("hooks.json.bak.{}", stamp));
        let _ = fs::copy(path, &bk);
        Some(bk)
    } else {
        None
    };

    for (event, cmd, matcher) in &to_apply {
        let mut entries = config[event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_winmux_entry(e));
        entries.push(json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": cmd }]
        }));
        config[event.as_str()] = json!(entries);
    }

    let text = serde_json::to_string_pretty(&config).unwrap_or_default();
    let _ = fs::write(path, text);

    AgentStatus::Done {
        registered: to_apply.into_iter().map(|(e, _, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
        hooks_version: Some(spec.winmux_hooks_version.clone()),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_winmux_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|v| v.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("winmux") && c.contains("claude-hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub struct Stub {
    pub label: &'static str,
}

impl HookAdapter for Stub {
    fn label(&self) -> &'static str {
        self.label
    }
    fn run(&self, _dry: bool, _force: bool, _source: &str) -> AgentStatus {
        AgentStatus::Stub("not yet implemented".into())
    }
}

pub fn run_all(adapters: &[Box<dyn HookAdapter>], dry: bool, force: bool, source: &str) {
    for a in adapters {
        let s = a.run(dry, force, source);
        print_status(a.label(), &s);
    }
    if dry {
        println!("Done. (dry-run, no writes)");
    } else {
        println!("Done. Restart your agent for hooks to take effect.");
    }
}

fn print_status(label: &str, status: &AgentStatus) {
    match status {
        AgentStatus::NotDetected => {
            println!("✗ {}: not detected (skipped)", label);
        }
        AgentStatus::Stub(reason) => {
            println!("✗ {}: {}", label, reason);
        }
        AgentStatus::Error(e) => {
            println!("✗ {}: error — {}", label, e);
        }
        AgentStatus::DryRun {
            would_register,
            path,
            already_present,
        } => {
            println!("✓ {} detected", label);
            if !already_present.is_empty() {
                println!(
                    "  → already present (would skip): {}",
                    already_present.join(", ")
                );
            }
            if would_register.is_empty() {
                println!("  → all hooks already present, would skip ({})", path.display());
            } else {
                println!(
                    "  → would register: {} (target: {})",
                    would_register.join(", "),
                    path.display()
                );
                println!("  → would create a timestamped .bak before writing");
            }
        }
        AgentStatus::Done {
            registered,
            backup,
            path,
            unchanged,
            hooks_version,
        } => {
            println!("✓ {} detected", label);
            if *unchanged {
                println!("  → all hooks already present, nothing to do ({})", path.display());
            } else {
                println!(
                    "  → registered: {} (target: {})",
                    registered.join(", "),
                    path.display()
                );
                if let Some(bk) = backup {
                    println!("  → backed up previous to {}", bk.display());
                }
            }
            if let Some(v) = hooks_version {
                println!("  → winmux_hooks_version = {v}");
            }
        }
    }
}
