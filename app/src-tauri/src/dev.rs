// Phase 8.E: introspection helpers for `winmux dev`. The Tauri commands and
// RPC handlers live alongside the rest of the app — this module owns just the
// shared data structures and pure helpers (state-snapshot building, console
// ring buffer, log/bug-report file IO).

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const CONSOLE_MAX: usize = 200;

#[derive(Clone, Serialize)]
pub(crate) struct ConsoleEntry {
    pub level: String,
    pub message: String,
    pub ts: i64,
}

pub(crate) type ConsoleBuffer = Arc<Mutex<VecDeque<ConsoleEntry>>>;

pub(crate) fn push_console(buf: &ConsoleBuffer, entry: ConsoleEntry) {
    let mut q = buf.lock().unwrap();
    q.push_back(entry);
    while q.len() > CONSOLE_MAX {
        q.pop_front();
    }
}

pub(crate) fn console_tail(buf: &ConsoleBuffer, limit: usize) -> Vec<ConsoleEntry> {
    let q = buf.lock().unwrap();
    let take = limit.min(q.len());
    q.iter().rev().take(take).rev().cloned().collect()
}

/// Tail the last `n` lines of a UTF-8 log file. Reads only the trailing slice
/// (best-effort — for very small N relative to file size, we still read the
/// whole file because debug.log is line-formatted and tiny in practice).
pub(crate) fn debug_log_tail(path: &Path, n: usize) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Build the `dev.get-state` payload. Pure function over the relevant state
/// shards plus the log/console paths. Caller is responsible for taking the
/// AppState locks and passing the cloned data in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_state_value(
    version: &str,
    git_hash: &str,
    build_time_unix: u64,
    appdata_dir: &Path,
    workspaces: &crate::WorkspacesFile,
    sessions_summary: Vec<SessionSummary>,
    forwards_summary: Vec<ForwardSummary>,
    feed_counts: FeedCounts,
    notes_counts: NotesCounts,
    log_tail_lines: Vec<String>,
    console_tail_entries: Vec<ConsoleEntry>,
) -> Value {
    let mut by_id: Map<String, Value> = Map::new();
    for ws in &workspaces.workspaces {
        let mut breakdown: HashMap<&str, u32> = HashMap::new();
        let mut pane_count: u32 = 0;
        if let Some(layout) = &ws.layout {
            crate::collect_panes_with_kind(layout, &mut |kind| {
                pane_count += 1;
                let key = match kind {
                    crate::PaneKind::Terminal => "terminal",
                    crate::PaneKind::Browser => "browser",
                    crate::PaneKind::FileManager => "file_manager",
                    crate::PaneKind::ClaudeChat => "claude_chat",
                    crate::PaneKind::ClaudeLog => "claude_log",
                };
                *breakdown.entry(key).or_insert(0) += 1;
            });
        }
        by_id.insert(
            ws.id.clone(),
            json!({
                "name": ws.name,
                "pane_count": pane_count,
                "kind_breakdown": breakdown,
            }),
        );
    }

    json!({
        "version": version,
        "git_hash": git_hash,
        "build_time_unix": build_time_unix,
        "appdata_dir": appdata_dir.to_string_lossy(),
        "workspaces": {
            "count": workspaces.workspaces.len(),
            "active_id": workspaces.active_workspace_id,
            "by_id": by_id,
        },
        "sessions": {
            "active": sessions_summary,
        },
        "tunnels": {
            "forwards": forwards_summary,
            "rpc_forwards": [],
        },
        "feed": {
            "open": feed_counts.open,
            "done": feed_counts.done,
            "by_kind": feed_counts.by_kind,
        },
        "notes": {
            "open": notes_counts.open,
            "done": notes_counts.done,
            "by_tag": notes_counts.by_tag,
        },
        "log_tail": log_tail_lines,
        "console_tail": console_tail_entries,
    })
}

#[derive(Serialize)]
pub(crate) struct SessionSummary {
    pub pane_id: String,
    pub kind: String,
    pub connection_type: Option<String>,
    pub workspace_id: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ForwardSummary {
    pub workspace_id: String,
    pub remote_port: u16,
    pub local_port: u16,
}

#[derive(Default)]
pub(crate) struct FeedCounts {
    pub open: u32,
    pub done: u32,
    pub by_kind: HashMap<String, u32>,
}

#[derive(Default)]
pub(crate) struct NotesCounts {
    pub open: u32,
    pub done: u32,
    pub by_tag: HashMap<String, u32>,
}

/// Write a bug report blob to `<appdata>/winmux/bug-reports/<ts>.json`. Returns
/// the absolute path written. The caller assembles the Value (state + free-form
/// description fields).
pub(crate) fn write_bug_report(appdata_dir: &Path, body: &Value) -> Result<PathBuf, String> {
    let dir = appdata_dir.join("bug-reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Lightweight ISO-ish stamp without pulling in chrono just for this:
    // YYYYMMDD-HHMMSS in UTC. Falls back to the unix timestamp if we can't
    // format manually (we never can without chrono, so just use the raw stamp).
    let path = dir.join(format!("bug-{ts}.json"));
    let pretty =
        serde_json::to_string_pretty(body).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, &pretty).map_err(|e| format!("write {:?}: {e}", path))?;
    Ok(path)
}
