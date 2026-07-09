//! Phase 78 — Claude subscription-usage fetch.
//!
//! `claude -p "/usage" --output-format json` returns the user's REAL Pro/Max
//! subscription quota (session %, weekly %, per-model %, reset times, and a
//! "what's contributing" breakdown) in the JSON envelope's `result` string.
//! The call is FREE (`total_cost_usd: 0`, `num_turns: 0`) but ~8 s latency
//! (a real round-trip), so we cache per-workspace for 5 min and only fetch
//! on demand / on a slow auto-refresh — never fast-poll.
//!
//! Rule #1: we log only the workspace id + percentages, never the `/usage`
//! body (it names the user's subagents / skills / MCP servers).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::State;

use crate::AppState;

/// One model's weekly usage row (e.g. `Current week (Fable): 16% used …`).
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ModelUsage {
    pub name: String,
    pub pct: u8,
    pub reset: String,
}

/// Parsed `/usage` snapshot for one workspace's Claude account.
#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
pub(crate) struct ClaudeUsage {
    pub session_pct: u8,
    pub session_reset: String,
    pub week_pct: u8,
    pub week_reset: String,
    /// Per-model weekly rows (excludes the "all models" aggregate).
    pub models: Vec<ModelUsage>,
    /// Raw "Last 24h" contributing lines (header + indented bullets).
    pub contributing_24h: Vec<String>,
    /// Raw "Last 7d" contributing lines.
    pub contributing_7d: Vec<String>,
    pub fetched_unix: i64,
}

// ─── cache (per workspace, 5-min TTL) — mirrors updater.rs ───────────────────
static USAGE_CACHE: Mutex<Option<HashMap<String, (Instant, ClaudeUsage)>>> = Mutex::new(None);
const USAGE_CACHE_TTL: Duration = Duration::from_secs(300);

fn cache_fresh(workspace_id: &str, force: bool) -> Option<ClaudeUsage> {
    if force {
        return None;
    }
    let guard = USAGE_CACHE.lock().ok()?;
    let (at, usage) = guard.as_ref()?.get(workspace_id)?;
    if at.elapsed() < USAGE_CACHE_TTL {
        Some(usage.clone())
    } else {
        None
    }
}

fn cache_stale(workspace_id: &str) -> Option<ClaudeUsage> {
    let guard = USAGE_CACHE.lock().ok()?;
    guard.as_ref()?.get(workspace_id).map(|(_, u)| u.clone())
}

fn cache_store(workspace_id: &str, usage: &ClaudeUsage) {
    if let Ok(mut guard) = USAGE_CACHE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(workspace_id.to_string(), (Instant::now(), usage.clone()));
    }
}

// ─── parsing ─────────────────────────────────────────────────────────────────

/// `"33% used · resets Jul 8, 4:10am (Europe/Berlin)"` → `(33, "Jul 8, 4:10am (Europe/Berlin)")`.
fn pct_and_reset(rest: &str) -> (u8, String) {
    let pct = rest
        .split('%')
        .next()
        .unwrap_or("")
        .trim()
        .parse::<u8>()
        .unwrap_or(0);
    let reset = rest
        .split("resets ")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    (pct, reset)
}

/// Extract the `result` text from the JSON envelope, then line-scan it.
fn parse_usage(raw: &str, now_unix: i64) -> Result<ClaudeUsage, String> {
    let text = serde_json::from_str::<serde_json::Value>(raw.trim())
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .ok_or("could not parse /usage output (claude installed & authenticated?)")?;

    let mut session_pct = 0u8;
    let mut session_reset = String::new();
    let mut week_pct = 0u8;
    let mut week_reset = String::new();
    let mut models: Vec<ModelUsage> = Vec::new();
    let mut c24: Vec<String> = Vec::new();
    let mut c7: Vec<String> = Vec::new();
    let mut found_session = false;

    enum Mode {
        None,
        Day,
        Week,
    }
    let mut mode = Mode::None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Current session:") {
            let (p, r) = pct_and_reset(rest);
            session_pct = p;
            session_reset = r;
            found_session = true;
            mode = Mode::None;
        } else if let Some(rest) = trimmed.strip_prefix("Current week (all models):") {
            let (p, r) = pct_and_reset(rest);
            week_pct = p;
            week_reset = r;
            mode = Mode::None;
        } else if trimmed.starts_with("Current week (") {
            // Per-model: "Current week (<Name>): NN% used · resets …"
            if let Some(name) = trimmed
                .split_once('(')
                .and_then(|(_, r)| r.split_once("):"))
                .map(|(name, _)| name.to_string())
            {
                if let Some((_, rest)) = trimmed.split_once("): ") {
                    let (p, r) = pct_and_reset(rest);
                    models.push(ModelUsage { name, pct: p, reset: r });
                }
            }
            mode = Mode::None;
        } else if trimmed.starts_with("Last 24h") {
            mode = Mode::Day;
            c24.push(trimmed.to_string());
        } else if trimmed.starts_with("Last 7d") {
            mode = Mode::Week;
            c7.push(trimmed.to_string());
        } else if trimmed.is_empty() {
            mode = Mode::None;
        } else if line.starts_with("  ") {
            // Indented bullet under the active "Last …" header.
            match mode {
                Mode::Day => c24.push(trimmed.to_string()),
                Mode::Week => c7.push(trimmed.to_string()),
                Mode::None => {}
            }
        } else {
            mode = Mode::None;
        }
    }

    if !found_session {
        return Err("unexpected /usage format — no session line".into());
    }
    Ok(ClaudeUsage {
        session_pct,
        session_reset,
        week_pct,
        week_reset,
        models,
        contributing_24h: c24,
        contributing_7d: c7,
        fetched_unix: now_unix,
    })
}

// ─── command ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn claude_usage_fetch(
    state: State<'_, AppState>,
    workspace_id: String,
    force: bool,
) -> Result<ClaudeUsage, String> {
    if let Some(cached) = cache_fresh(&workspace_id, force) {
        return Ok(cached);
    }
    let handle = crate::addons::pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    // Login shell so `claude` is on PATH in a non-interactive session; stderr
    // dropped so `result` stays clean JSON.
    let out = crate::addons::exec(
        &handle,
        "bash -lc 'claude -p \"/usage\" --output-format json 2>/dev/null'",
        20,
    )
    .await?;
    let now = now_unix();
    match parse_usage(&out, now) {
        Ok(usage) => {
            crate::dlog_tag(
                "USAGE",
                &format!(
                    "workspace={workspace_id} session={}% week={}%",
                    usage.session_pct, usage.week_pct
                ),
            );
            cache_store(&workspace_id, &usage);
            Ok(usage)
        }
        // Serve stale on a transient parse/fetch miss before surfacing the error.
        Err(e) => {
            crate::dlog_tag("USAGE", &format!("workspace={workspace_id} unavailable"));
            cache_stale(&workspace_id).ok_or(e)
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"You are currently using your subscription to power your Claude Code usage\n\nCurrent session: 33% used · resets Jul 8, 4:10am (Europe/Berlin)\nCurrent week (all models): 11% used · resets Jul 14, 10pm (Europe/Berlin)\nCurrent week (Fable): 16% used · resets Jul 14, 10pm (Europe/Berlin)\n\nWhat's contributing to your limits usage?\nApproximate, based on local sessions on this machine.\n\nLast 24h · 3466 requests · 10 sessions\n  94% of your usage came from subagent-heavy sessions\n  Top subagents: implementer 40%, loop 8%\n\nLast 7d · 13897 requests · 26 sessions\n  99% of your usage came from subagent-heavy sessions","total_cost_usd":0}"#;

    #[test]
    fn parses_real_usage() {
        let u = parse_usage(SAMPLE, 1_700_000_000).expect("should parse");
        assert_eq!(u.session_pct, 33);
        assert_eq!(u.session_reset, "Jul 8, 4:10am (Europe/Berlin)");
        assert_eq!(u.week_pct, 11);
        assert_eq!(u.models.len(), 1);
        assert_eq!(u.models[0].name, "Fable");
        assert_eq!(u.models[0].pct, 16);
        assert_eq!(u.contributing_24h.len(), 3); // header + 2 bullets
        assert_eq!(u.contributing_7d.len(), 2); // header + 1 bullet
        assert_eq!(u.fetched_unix, 1_700_000_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_usage("not json", 0).is_err());
        assert!(parse_usage(r#"{"result":"hello world"}"#, 0).is_err());
    }
}
