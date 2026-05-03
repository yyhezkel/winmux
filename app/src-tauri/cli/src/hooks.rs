//! `winmux setup-hooks` — register agent hooks that point at the local winmux CLI
//! so AI coding agents (Claude Code etc.) can pipe permission requests / lifecycle
//! events back through the tunnel into the Windows app's UI.
//!
//! Designed to be idempotent and additive: existing entries unrelated to winmux are
//! preserved, and our entries are detected by a `winmux ... claude-hook` substring
//! match so we replace ourselves instead of accumulating duplicates.

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
    fn run(&self, dry: bool, force: bool) -> AgentStatus;
}

/// Map of (Claude Code event name → our `claude-hook <subcmd>`).
///
/// The blocking subcommands (`pre-tool-use`) are determined server-side based on
/// the subcommand string; see `Cmd::ClaudeHook` in `main.rs`.
const CLAUDE_EVENTS: &[(&str, &str)] = &[
    ("PreToolUse", "pre-tool-use"),
    ("Notification", "notification"),
    ("SessionStart", "session-start"),
    ("SessionEnd", "session-end"),
    ("Stop", "stop"),
];

pub struct Claude;

impl HookAdapter for Claude {
    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn run(&self, dry: bool, force: bool) -> AgentStatus {
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

        // Phase setup-hooks-fix: current Claude Code reads hooks from
        // `~/.claude/settings.json` under a top-level `"hooks"` key — NOT
        // from a separate `~/.claude/hooks.json`. We write BOTH so:
        //   • settings.json is what Claude Code actually consumes,
        //   • hooks.json stays for any legacy tooling that might still read it.
        // settings.json is shared with non-hook config (theme, etc.) — we
        // read-modify-write only the `hooks` subtree.

        let settings_path = claude_dir.join("settings.json");
        let legacy_path = claude_dir.join("hooks.json");

        // settings.json is the load-bearing target. Its outcome drives the
        // status surface. The legacy file is updated on a best-effort basis.
        let settings_outcome =
            apply_to_settings(&claude_dir, &settings_path, &exe_path, dry, force);
        let _ = apply_to_legacy(&claude_dir, &legacy_path, &exe_path, dry, force);
        settings_outcome
    }
}

/// Writes our hooks under `["hooks"][event]` in `~/.claude/settings.json`,
/// preserving every other key. Atomic + timestamped backup.
fn apply_to_settings(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
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
    let mut to_apply: Vec<(String, String)> = Vec::new();

    for (event, subcmd) in CLAUDE_EVENTS {
        let cmd = format!("{} claude-hook {}", exe_path, subcmd);
        let entries = root["hooks"][event]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_winmux = entries.iter().any(is_winmux_entry);
        if has_winmux && !force {
            already_present.push((*event).into());
            continue;
        }
        would_register.push((*event).into());
        to_apply.push(((*event).into(), cmd));
    }

    if dry {
        return AgentStatus::DryRun {
            would_register,
            path: path.to_path_buf(),
            already_present,
        };
    }

    if to_apply.is_empty() {
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
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

    for (event, cmd) in &to_apply {
        let mut entries = root["hooks"][event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_winmux_entry(e));
        entries.push(json!({
            "matcher": "*",
            "hooks": [{ "type": "command", "command": cmd }]
        }));
        root["hooks"][event.as_str()] = json!(entries);
    }

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
        registered: to_apply.into_iter().map(|(e, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
    }
}

/// Best-effort write to the legacy top-level `~/.claude/hooks.json` (the
/// shape Claude Code USED to read). Modern Claude Code ignores this file;
/// kept so any third-party tooling that still scrapes it stays in sync.
fn apply_to_legacy(
    claude_dir: &std::path::Path,
    path: &std::path::Path,
    exe_path: &str,
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

    let mut to_apply: Vec<(String, String)> = Vec::new();
    for (event, subcmd) in CLAUDE_EVENTS {
        let cmd = format!("{} claude-hook {}", exe_path, subcmd);
        let entries = config[event].as_array().cloned().unwrap_or_default();
        let has_winmux = entries.iter().any(is_winmux_entry);
        if has_winmux && !force {
            continue;
        }
        to_apply.push(((*event).into(), cmd));
    }
    if dry || to_apply.is_empty() {
        return AgentStatus::Done {
            registered: vec![],
            backup: None,
            path: path.to_path_buf(),
            unchanged: true,
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

    for (event, cmd) in &to_apply {
        let mut entries = config[event.as_str()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.retain(|e| !is_winmux_entry(e));
        entries.push(json!({
            "matcher": "*",
            "hooks": [{ "type": "command", "command": cmd }]
        }));
        config[event.as_str()] = json!(entries);
    }

    let tmp = claude_dir.join(format!("hooks.json.winmux-tmp.{}", std::process::id()));
    let text = serde_json::to_string_pretty(&config).unwrap_or_default();
    let _ = fs::write(&tmp, &text);
    let _ = fs::rename(&tmp, path);

    AgentStatus::Done {
        registered: to_apply.into_iter().map(|(e, _)| e).collect(),
        backup,
        path: path.to_path_buf(),
        unchanged: false,
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
    fn run(&self, _dry: bool, _force: bool) -> AgentStatus {
        AgentStatus::Stub("not yet implemented".into())
    }
}

pub fn run_all(adapters: &[Box<dyn HookAdapter>], dry: bool, force: bool) {
    for a in adapters {
        let s = a.run(dry, force);
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
                if let Some(b) = backup {
                    println!("  → backup: {}", b.display());
                }
            }
        }
    }
}
