use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::notes::{self, NoteStatus};
use crate::dev;
use crate::settings;
use crate::updater;
use crate::{
    collect_panes, collect_panes_with_kind, config_dir_pub, decide_feed,
    find_workspace_for_pane, new_pane_id, new_workspace_id,
    persist, split_pane_in,
    update_pane_in, write_to_session, AppState, Connection, CreateInput, EnvVar, FeedItem,
    FeedItemState, LayoutNode, NotificationItem, PaneKind, Session, SplitDirection, Workspace,
    NOTIF_COUNTER,
};

const FEED_MAX_ITEMS_LIMIT: usize = 50;

// Phase 51.C: pipe_name moved to winmux-core (shared with winmux-tunnel).
pub use winmux_core::pipe_name;

// Phase 39.A: removed the 8-cap that caused ERROR_PIPE_NOT_AVAILABLE
// storms under concurrent RPC.
// Phase 39.C: 254 is the tokio wrapper's hard maximum —
// `ServerOptions::max_instances(255)` PANICS at construction with
// "cannot specify more than 254 instances", which left the pipe server
// dead and every tunnel bridge hitting ERROR_FILE_NOT_FOUND. We also
// wrap the builder in catch_unwind so a future tokio-version change to
// the limit degrades to a logged fallback instead of crashing the
// server task.
const PIPE_MAX_INSTANCES: usize = 254;

fn make_listener(name: &str) -> Result<NamedPipeServer, String> {
    use tokio::net::windows::named_pipe::PipeMode;
    let build = |max: usize| {
        ServerOptions::new()
            .pipe_mode(PipeMode::Byte)
            .first_pipe_instance(false)
            .max_instances(max)
            .create(name)
    };
    match std::panic::catch_unwind(|| build(PIPE_MAX_INSTANCES)) {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(format!("create pipe: {e}")),
        Err(_) => {
            crate::dlog(&format!(
                "rpc_server: max_instances({PIPE_MAX_INSTANCES}) panicked, falling back to 100"
            ));
            build(100).map_err(|e| format!("fallback create pipe: {e}"))
        }
    }
}

// Phase 44: pool of LISTENER_POOL_SIZE concurrent listeners always in
// accept state. Phase 39.A only ever had ONE listener listening at a
// time (it pre-created the NEXT before spawning the handler, but still
// only one at any instant), so two clients racing for the slot within
// microseconds raced and one got ERROR_PIPE_NOT_AVAILABLE (231). A pool
// of 8 absorbs the bursts seen in practice (port.opened + a hook event
// arriving together); the 9th+ falls back to tunnel.rs's bounded
// backoff. max_instances(254) ceiling unchanged. Each slot owns its
// loop; handlers run on a separate task so the slot recreates its
// listener immediately, not after the handler completes.
const LISTENER_POOL_SIZE: usize = 8;

pub async fn run(state: AppState, app: AppHandle) {
    let name = pipe_name();
    tracing::info!(
        "rpc: listening on {} (pool of {} listeners)",
        name,
        LISTENER_POOL_SIZE
    );
    spawn_listener_pool(name, LISTENER_POOL_SIZE, state, app);
}

fn spawn_listener_pool(name: String, size: usize, state: AppState, app: AppHandle) {
    for slot in 0..size {
        let name = name.clone();
        let state = state.clone();
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                let listener = match make_listener(&name) {
                    Ok(l) => l,
                    Err(e) => {
                        crate::dlog(&format!(
                            "rpc_server: pool slot {slot} make_listener failed: {e} — retrying in 500ms"
                        ));
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                };
                match listener.connect().await {
                    Ok(()) => {
                        // Hand off on a separate task so this slot can
                        // recreate its listener immediately, never blocked
                        // on handler duration.
                        let state2 = state.clone();
                        let app2 = app.clone();
                        tokio::spawn(handle_client_with_telemetry(listener, state2, app2));
                    }
                    Err(e) => {
                        crate::dlog(&format!(
                            "rpc_server: pool slot {slot} connect failed: {e}"
                        ));
                    }
                }
            }
        });
    }
}

// Phase 44: wrap the handler with start/end + elapsed-ms telemetry so
// slow handlers surface in debug.log without needing a profiler. The
// handler itself is unchanged — it loops over JSON-RPC lines and exits
// on EOF or read error, which we treat uniformly as "ended".
// Phase 48-C: handler-served counter lifted to module-scope so
// `doctor` can include it in the diagnostic snapshot. Each new
// pipe connection increments this; the value is monotonic for the
// process's lifetime.
pub(crate) static HANDLER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn handle_client_with_telemetry(
    stream: NamedPipeServer,
    state: AppState,
    app: AppHandle,
) {
    use std::sync::atomic::Ordering;
    let conn_id = format!("{:05x}", HANDLER_SEQ.fetch_add(1, Ordering::Relaxed));
    let start = std::time::Instant::now();
    crate::dlog(&format!("rpc_server: handler {conn_id} START"));
    handle_client(stream, state, app).await;
    let elapsed_ms = start.elapsed().as_millis();
    crate::dlog(&format!(
        "rpc_server: handler {conn_id} END {elapsed_ms} ms"
    ));
}

/// v0.3.1 (pipe-instance-leak fix): the pipe RPC protocol is **one request per
/// connection** — a hook fires, sends one feed.push line, reads one reply, and
/// is done. Previously `handle_client` looped waiting for a *next* request that
/// never comes, and the tunnel bridge's `copy_bidirectional` does NOT propagate
/// the remote CLI's channel-close to the local pipe (the russh ChannelStream
/// doesn't surface EOF here), so the handler blocked in `read_line` FOREVER —
/// never dropping the `NamedPipeServer`, leaking one of the 254 `max_instances`.
/// Phase 66 made hooks fire on every Claude tool call, so the pipe exhausted
/// within ~254 calls and ERROR_PIPE_BUSY (231) wedged every connection.
///
/// Fix: serve exactly one request, then return — the stream drops, the pipe
/// instance is freed immediately, and the bridge's `copy_bidirectional`
/// unblocks (it sees the pipe EOF). The bounded first-read guards a client that
/// connects but never sends. A blocking permission decision runs inside
/// `dispatch` (after the read), so gated requests are unaffected.
const HANDLER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn handle_client(stream: NamedPipeServer, state: AppState, app: AppHandle) {
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;
    let mut line = String::new();
    // Read the single request line (bounded so a silent client can't pin the
    // instance). Anything other than a non-empty line → nothing to serve.
    match tokio::time::timeout(HANDLER_READ_TIMEOUT, reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => {}
        _ => return,
    }
    let resp = match serde_json::from_str::<Value>(line.trim()) {
        Ok(req) => {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let method = req
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let params = req.get("params").cloned().unwrap_or(json!({}));
            match dispatch(&method, params, &state, &app).await {
                Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32000, "message": e }
                }),
            }
        }
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": format!("parse error: {e}") }
        }),
    };
    let _ = writer.write_all(format!("{resp}\n").as_bytes()).await;
    let _ = writer.flush().await;
    // Return → `stream` (split halves) drop → pipe instance freed immediately.
}

fn translate_key(key: &str) -> Vec<u8> {
    match key.to_lowercase().as_str() {
        "enter" | "return" | "cr" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "ctrl-c" | "ctrl+c" | "^c" => b"\x03".to_vec(),
        "ctrl-d" | "ctrl+d" | "^d" => b"\x04".to_vec(),
        "ctrl-z" | "ctrl+z" | "^z" => b"\x1a".to_vec(),
        "ctrl-l" | "ctrl+l" | "^l" => b"\x0c".to_vec(),
        "esc" | "escape" => b"\x1b".to_vec(),
        "backspace" | "bs" => b"\x7f".to_vec(),
        "up" | "arrow-up" => b"\x1b[A".to_vec(),
        "down" | "arrow-down" => b"\x1b[B".to_vec(),
        "right" | "arrow-right" => b"\x1b[C".to_vec(),
        "left" | "arrow-left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        other => other.as_bytes().to_vec(),
    }
}

fn show_toast(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let r = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .appname("winmux")
            .show();
        if let Err(e) = r {
            tracing::warn!("toast failed: {e}");
        }
    });
}

/// Phase 66 (KK): truncate a string to `max` chars with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Phase 66 (KK): turn a raw hook payload into a friendly (title, body)
/// toast instead of dumping the JSON. `subkind` is the hook event
/// (session-start / session-end / stop / notification / pre-tool-use).
/// `lang` is the UI language (he/ar/ru/en) — Hebrew is first-class; ar/ru
/// fall back to English for the toast text (the Settings labels are fully
/// translated separately).
fn humanize_notification(subkind: &str, payload: &Value, lang: &str) -> (String, String) {
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let tool = payload.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let cmd = payload
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let msg = payload.get("message").and_then(|v| v.as_str()).unwrap_or("");
    // v0.4.4: Stop carries `response_summary`; SessionEnd carries
    // `session_duration_seconds` + `end_reason`.
    let summary = payload
        .get("response_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let dur_secs = payload
        .get("session_duration_seconds")
        .and_then(|v| v.as_u64());
    let he = lang == "he";
    let fmt_dur = |secs: u64| -> String {
        let m = secs / 60;
        let s = secs % 60;
        if m > 0 { format!("{m}m {s}s") } else { format!("{s}s") }
    };
    match subkind {
        "session-start" => (
            "Claude".into(),
            if he { format!("סשן התחיל ב-{cwd}") } else { format!("Session started in {cwd}") },
        ),
        "session-end" => (
            if he { "🎯 Session נסגר".into() } else { "🎯 Session ended".into() },
            match dur_secs {
                Some(d) if he => format!("{cwd} · משך {}", fmt_dur(d)),
                Some(d) => format!("{cwd} · {} elapsed", fmt_dur(d)),
                None if he => format!("סשן הסתיים — {cwd}"),
                None => format!("Session ended — {cwd}"),
            },
        ),
        "stop" => (
            if he { "🎯 Claude סיים — התור שלך".into() } else { "🎯 Claude finished — your turn".into() },
            if !summary.is_empty() {
                clip(summary, 120)
            } else if he {
                format!("סיים ב-{cwd}")
            } else {
                format!("Finished in {cwd}")
            },
        ),
        "notification" => (
            "Claude".into(),
            if !msg.is_empty() {
                msg.to_string()
            } else if he {
                "Claude זקוק לך".into()
            } else {
                "Claude needs you".into()
            },
        ),
        "pre-tool-use" => (
            if he { format!("Claude רוצה להריץ: {tool}") } else { format!("Claude wants to run: {tool}") },
            clip(cmd, 100),
        ),
        _ => ("Claude".into(), String::new()),
    }
}

/// Phase 66 (KK): is a toast wanted for this hook event, per the
/// per-event Notifications toggles? `toast_enabled` is the master switch.
fn hook_toast_enabled(n: &settings::Notifications, subkind: &str) -> bool {
    if !n.toast_enabled {
        return false;
    }
    match subkind {
        "session-start" => n.toast_session_start,
        "session-end" => n.toast_session_end,
        "stop" => n.toast_stop,
        "notification" => n.toast_notification,
        "pre-tool-use" => n.toast_gate,
        _ => false,
    }
}

/// Phase 66 (66.D): record a passive feed item for a policy decision that
/// resolved WITHOUT a card (an Auto allow or a Block deny). Gives the user
/// an audit trail in the feed — "what did winmux silently allow / block?"
/// — without ever blocking the agent. Mirrors the passive-item registration
/// in the feed.push handler.
fn push_policy_audit(
    state: &AppState,
    app: &AppHandle,
    req_id: &str,
    subkind: &str,
    title: &str,
    summary: &str,
    pane_id: Option<String>,
    workspace_id: Option<String>,
) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let item = FeedItem {
        request_id: req_id.to_string(),
        kind: "passive".to_string(),
        subkind: subkind.to_string(),
        pane_id,
        workspace_id,
        title: title.to_string(),
        summary: summary.to_string(),
        payload: json!({}),
        state: FeedItemState::Passive,
        created_ms: now_ms,
        blocking: false,
    };
    {
        let mut store = state.feed.lock().unwrap();
        store.items.push_back(item.clone());
        while store.items.len() > FEED_MAX_ITEMS_LIMIT {
            store.items.pop_front();
        }
    }
    let _ = app.emit("feed:item-added", &item);
}

// Phase 53 (rebased): the `kind: "browser"` arm of `pane.split`
// remains so an older CLI / agent script that still passes that
// string still works — the spawned pane gets the deprecated
// Browser kind and is rewritten to Terminal on the next restart by
// the load-time migration. Frontend split menu no longer exposes
// "browser". `#[allow(deprecated)]` covers that arm and the
// fold_pane_kinds helper below.
#[allow(deprecated)]
async fn dispatch(
    method: &str,
    params: Value,
    state: &AppState,
    app: &AppHandle,
) -> Result<Value, String> {
    match method {
        // Phase 66 (66.D.2): lightweight liveness probe. The remote
        // claude-hook pings this before a blocking permission request so it
        // can fall back to its static policy fast when the desktop is
        // unreachable, instead of stalling the agent on the full timeout.
        "ping" => Ok(json!({ "ok": true })),
        "list-workspaces" => {
            let file = state.workspaces.lock().unwrap().clone();
            serde_json::to_value(&file).map_err(|e| e.to_string())
        }

        "select-workspace" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            {
                let mut file = state.workspaces.lock().unwrap();
                if !file.workspaces.iter().any(|w| w.id == id) {
                    return Err(format!("no workspace {id}"));
                }
                file.active_workspace_id = Some(id.clone());
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "active": id }))
        }

        "new-workspace" => {
            let input: CreateInput =
                serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            let ws = Workspace {
                id: new_workspace_id(),
                name: input.name,
                color: input.color,
                emoji: None,
                cwd: input.cwd,
                connection: None,
                layout: Some(LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: PaneKind::Terminal,
                    connection: Some(input.connection),
                    browser: None,
                    title: None,
                    annotation: None,
                    color: None,
                    emoji: None,
                    help_topic: None,
                    diff_source: None,
                    smart_bidi: None,
                }),
                setup_command: input.setup_command,
                teardown_command: input.teardown_command,
                env: input.env.unwrap_or_default(),
                auto_port_forward: false,
                last_active_at: 0,
                git_worktree: None,
                claude_separate_account: false,
                // cmux-A A2: RPC-created workspaces default to ungrouped.
                group_id: None,
            };
            let cloned = ws.clone();
            {
                let mut file = state.workspaces.lock().unwrap();
                file.active_workspace_id = Some(ws.id.clone());
                file.workspaces.push(ws);
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            serde_json::to_value(&cloned).map_err(|e| e.to_string())
        }

        "update-workspace" => {
            let workspace_id = params
                .get("workspace_id")
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from);
            let color = params
                .get("color")
                .and_then(|v| v.as_str())
                .map(String::from);
            let cwd = params
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(String::from);
            let setup_command = params
                .get("setup_command")
                .or_else(|| params.get("setup"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let teardown_command = params
                .get("teardown_command")
                .or_else(|| params.get("teardown"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let env: Option<Vec<EnvVar>> = params
                .get("env")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            {
                let mut file = state.workspaces.lock().unwrap();
                let ws = file
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace_id)
                    .ok_or_else(|| format!("no workspace {workspace_id}"))?;
                if let Some(n) = name {
                    if !n.is_empty() {
                        ws.name = n;
                    }
                }
                if let Some(c) = color {
                    ws.color = if c.is_empty() { None } else { Some(c) };
                }
                if let Some(d) = cwd {
                    ws.cwd = if d.is_empty() { None } else { Some(d) };
                }
                if let Some(s) = setup_command {
                    ws.setup_command = if s.is_empty() { None } else { Some(s) };
                }
                if let Some(t) = teardown_command {
                    ws.teardown_command = if t.is_empty() { None } else { Some(t) };
                }
                if let Some(e) = env {
                    ws.env = e;
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            let file = state.workspaces.lock().unwrap();
            let ws = file
                .workspaces
                .iter()
                .find(|w| w.id == workspace_id)
                .cloned();
            match ws {
                Some(w) => serde_json::to_value(&w).map_err(|e| e.to_string()),
                None => Ok(json!({ "ok": true })),
            }
        }

        "delete-workspace" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            let panes_to_kill: Vec<String> = {
                let file = state.workspaces.lock().unwrap();
                file.workspaces
                    .iter()
                    .find(|w| w.id == id)
                    .and_then(|w| w.layout.as_ref())
                    .map(|l| {
                        let mut v = Vec::new();
                        collect_panes(l, &mut v);
                        v
                    })
                    .unwrap_or_default()
            };
            for pane_id in &panes_to_kill {
                if let Some(sid) = state.core.pane_sessions.lock().unwrap().remove(pane_id) {
                    if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                        crate::kill_session_inner(&mut s);
                    }
                }
            }
            // Phase 8.B: tear down any port forwards for the workspace.
            crate::close_workspace_forwards(&state.core.forwards, &id);
            {
                let mut file = state.workspaces.lock().unwrap();
                file.workspaces.retain(|w| w.id != id);
                if file.active_workspace_id.as_deref() == Some(&id) {
                    file.active_workspace_id = file.workspaces.first().map(|w| w.id.clone());
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true }))
        }

        "send" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?;
            let data = params
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("missing data")?;
            let sid = state.core
                .pane_sessions
                .lock()
                .unwrap()
                .get(pane_id)
                .cloned()
                .ok_or_else(|| format!("pane {pane_id} not connected"))?;
            write_to_session(state, &sid, data.as_bytes())?;
            Ok(json!({ "ok": true, "bytes": data.len() }))
        }

        "send-key" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?;
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or("missing key")?;
            let bytes = translate_key(key);
            let sid = state.core
                .pane_sessions
                .lock()
                .unwrap()
                .get(pane_id)
                .cloned()
                .ok_or_else(|| format!("pane {pane_id} not connected"))?;
            write_to_session(state, &sid, &bytes)?;
            Ok(json!({ "ok": true, "bytes": bytes.len() }))
        }

        "notify" => {
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(no title)")
                .to_string();
            let body = params
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // #1: a coarse category for the Notification Center filter.
            // Hook/agent callers can pass "kind"; default "agent" since the
            // RPC notify channel is driven by Claude hooks.
            let kind = params
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("agent")
                .to_string();
            let item = NotificationItem {
                id: NOTIF_COUNTER.fetch_add(1, Ordering::Relaxed),
                title: title.clone(),
                body: body.clone(),
                workspace_id,
                timestamp_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
                kind,
            };
            state.notifications.lock().unwrap().push(item.clone());
            let _ = app.emit("notification:new", &item);
            show_toast(&title, &body);
            Ok(json!({ "ok": true, "id": item.id }))
        }

        "tree" => {
            let ws_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let file = state.workspaces.lock().unwrap().clone();
            let target = match ws_id.as_deref() {
                Some(id) => file.workspaces.iter().find(|w| w.id == id),
                None => file
                    .active_workspace_id
                    .as_deref()
                    .and_then(|id| file.workspaces.iter().find(|w| w.id == id)),
            };
            match target {
                Some(w) => Ok(json!({
                    "workspace_id": w.id,
                    "name": w.name,
                    "layout": w.layout
                })),
                None => Ok(Value::Null),
            }
        }

        // ─── B1: full LLM control over winmux ──────────────────────────
        // Six methods covering discovery + action + (best-effort) read
        // surface. Companion winmux-mcp tools wrap each one. Together
        // they let an agent running inside or outside winmux drive
        // workspace creation / connection, pane splitting, and key
        // injection, plus get a structured view of the current UI.

        // Mirror of list-workspaces with an "active" boolean per pane +
        // workspace, so agents can decide "which pane am I currently in"
        // without re-deriving from active_workspace_id.
        "ui.tree" => {
            let file = state.workspaces.lock().unwrap().clone();
            let active_id = file.active_workspace_id.clone();
            let workspaces: Vec<Value> = file
                .workspaces
                .iter()
                .map(|w| {
                    let mut panes: Vec<Value> = Vec::new();
                    if let Some(layout) = &w.layout {
                        fn walk(node: &LayoutNode, out: &mut Vec<Value>) {
                            match node {
                                LayoutNode::Pane {
                                    pane_id,
                                    pane_kind,
                                    title,
                                    annotation,
                                    connection,
                                    ..
                                } => {
                                    out.push(json!({
                                        "pane_id": pane_id,
                                        "kind": format!("{:?}", pane_kind).to_lowercase(),
                                        "title": title,
                                        "annotation": annotation,
                                        "connection": connection,
                                    }));
                                }
                                LayoutNode::Split { first, second, .. } => {
                                    walk(first, out);
                                    walk(second, out);
                                }
                            }
                        }
                        walk(layout, &mut panes);
                    }
                    json!({
                        "workspace_id": w.id,
                        "name": w.name,
                        "is_active": Some(&w.id) == active_id.as_ref(),
                        "connection": w.connection,
                        "panes": panes,
                    })
                })
                .collect();
            Ok(json!({
                "active_workspace_id": active_id,
                "workspaces": workspaces,
            }))
        }

        // Activate a workspace's UI tab + (if SSH) emit a request
        // for the FE to ensure_connected. We don't drive the SSH
        // handshake from this RPC because the headless path is
        // wired through a Tauri command surface — duplicating it
        // here would risk drift. The FE listens on workspaces:changed
        // and the active-workspace effect re-triggers connect.
        "action.connect" => {
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            {
                let mut file = state.workspaces.lock().unwrap();
                if !file.workspaces.iter().any(|w| w.id == workspace_id) {
                    return Err(format!("no workspace {workspace_id}"));
                }
                file.active_workspace_id = Some(workspace_id.clone());
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "active": workspace_id }))
        }

        // Split a pane in the workspace tree. Direction is
        // "horizontal" or "vertical"; kind defaults to terminal.
        // Mirrors workspace_split's split_pane_in usage but without
        // the four-tier fallback chain (RPC callers are agents, not
        // the wizard — they pass workspace_id explicitly).
        "action.split" => {
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("parent_pane_id"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id (or parent_pane_id)")?
                .to_string();
            let direction = match params
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("horizontal")
            {
                "horizontal" | "right" | "h" => SplitDirection::Horizontal,
                "vertical" | "down" | "v" => SplitDirection::Vertical,
                other => return Err(format!("bad direction: {other}")),
            };
            let fallback_conn: Option<Connection> = {
                let file = state.workspaces.lock().unwrap();
                file.workspaces
                    .iter()
                    .find(|w| w.id == workspace_id)
                    .and_then(|w| w.connection.clone())
            };
            let mut changed = false;
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        let (new_layout, did_split) = split_pane_in(
                            layout,
                            &pane_id,
                            direction,
                            PaneKind::Terminal,
                            None,
                            fallback_conn,
                            None,
                        );
                        ws.layout = Some(new_layout);
                        changed = did_split;
                    }
                }
            }
            if !changed {
                return Err(format!("split: pane {pane_id} not found"));
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "workspace_id": workspace_id, "split_from": pane_id }))
        }

        // Alias of `send-key` exposed under the canonical `action.*`
        // namespace so agents can use a consistent prefix. Same key
        // translation table; reuses translate_key.
        "action.send_keys" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?;
            let key = params
                .get("key")
                .or_else(|| params.get("keys"))
                .and_then(|v| v.as_str())
                .ok_or("missing key")?;
            let bytes = translate_key(key);
            let sid = state
                .core
                .pane_sessions
                .lock()
                .unwrap()
                .get(pane_id)
                .cloned()
                .ok_or_else(|| format!("pane {pane_id} not connected"))?;
            write_to_session(state, &sid, &bytes)?;
            Ok(json!({ "ok": true, "bytes": bytes.len() }))
        }

        // B1: scrollback. The backend does NOT buffer PTY output —
        // Absolute Rule #1 ("Never log PTY input or output content")
        // pushes the scrollback ring into the FRONTEND xterm.js, not
        // into Rust state. Returning the buffer to an RPC caller
        // would require a frontend round-trip we haven't built yet.
        // For tmux sessions there's a clean workaround: send
        // `tmux capture-pane -p -S -<lines>` to the pane via `send`
        // and read its output from the next pty:data event. Document
        // that in the error so agents have a path forward.
        "pane.scrollback" => {
            Err(
                "pane.scrollback: backend does not buffer PTY content (Absolute Rule #1). \
                 Workaround for tmux: rpc `send` with data \
                 `\\u001btmux capture-pane -p -S -<N>\\n` then read the next pty:data event."
                    .to_string(),
            )
        }

        // Same story for screenshots: xterm.js's canvas lives on the
        // frontend; rendering it requires the round-trip surface
        // Phase 53.G deleted. Returning a clean error is the honest
        // path until a frontend integration lands.
        "pane.screenshot" => {
            Err(
                "pane.screenshot: terminal canvas lives on the frontend (xterm.js) \
                 and requires a window→backend round-trip not built yet. Use pane.scrollback for text content."
                    .to_string(),
            )
        }

        "set-pane-title" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            // Empty string = clear; missing key = clear; non-empty = set.
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .map(String::from)
                .filter(|s| !s.is_empty());
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        ws.layout = Some(update_pane_in(layout, &pane_id, Some(title), None));
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true }))
        }

        "set-pane-annotation" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let annotation = params
                .get("annotation")
                .and_then(|v| v.as_str())
                .map(String::from)
                .filter(|s| !s.is_empty());
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        ws.layout =
                            Some(update_pane_in(layout, &pane_id, None, Some(annotation)));
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true }))
        }

        // Phase 8.A: split a pane (terminal or browser).
        "split" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let direction = match params
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("horizontal")
                .to_lowercase()
                .as_str()
            {
                "horizontal" | "right" | "h" => SplitDirection::Horizontal,
                "vertical" | "down" | "v" => SplitDirection::Vertical,
                other => return Err(format!("bad direction: {other}")),
            };
            let kind = match params.get("kind").and_then(|v| v.as_str()) {
                None | Some("terminal") => PaneKind::Terminal,
                Some("browser") => PaneKind::Browser,
                Some(other) => return Err(format!("bad kind: {other}")),
            };
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .map(String::from);
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            // Phase 23.C: compute the same workspace-level fallback
            // that workspace_split uses, so agent-driven splits via
            // RPC also inherit the workspace's SSH connection when
            // the source pane has none of its own.
            let fallback_conn: Option<crate::Connection> = if matches!(kind, PaneKind::Terminal) {
                // Phase 23.D: same four-tier fallback as workspace_split,
                // applied here so agent-driven splits over RPC also
                // inherit the canonical workspace connection.
                let (layout_fallback, ws_conn) = {
                    let file = state.workspaces.lock().unwrap();
                    let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
                    (
                        ws.and_then(|w| w.layout.as_ref().and_then(crate::first_terminal_connection_pub)),
                        ws.and_then(|w| w.connection.clone()),
                    )
                };
                layout_fallback
                    .or(ws_conn)
                    .or_else(|| crate::live_ssh_connection_for_workspace_pub(state, &workspace_id))
            } else {
                None
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        let (new_layout, _) = split_pane_in(
                            layout,
                            &pane_id,
                            direction,
                            kind,
                            url,
                            fallback_conn,
                            None,
                        );
                        ws.layout = Some(new_layout);
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "workspace_id": workspace_id }))
        }

        "set-status" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            state
                .pane_status
                .lock()
                .unwrap()
                .insert(pane_id.clone(), text.clone());
            let _ = app.emit("pane:status", json!({ "pane_id": pane_id, "text": text }));
            Ok(json!({ "ok": true }))
        }

        // ─── Phase 6.5: agent feed ────────────────────────────────────────
        "feed.push" => {
            let req_id = params
                .get("request_id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    format!("req_{:x}", now)
                });
            let kind = params
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("passive")
                .to_string();
            let subkind = params
                .get("subkind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let pane_id = params
                .get("pane_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(no title)")
                .to_string();
            let summary = params
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let payload = params.get("payload").cloned().unwrap_or(json!({}));
            // Clamp timeout to a sane range (1..=600s) regardless of what the
            // client requests, so a malicious or buggy CLI can't pin a
            // server thread/connection indefinitely.
            let timeout_secs = params
                .get("wait_timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(1, 600);
            crate::dlog(&format!(
                "feed.push: kind={} subkind={} timeout={}s req_id={}",
                params.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                params.get("subkind").and_then(|v| v.as_str()).unwrap_or(""),
                timeout_secs,
                params
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            ));

            let blocking = matches!(kind.as_str(), "permission_request");

            // ── Phase 66 (66.D): 3-state policy engine ──────────────────
            // Before a pre-tool-use request becomes a blocking approval
            // card, run the user's policy. This is the fix for the original
            // foot-gun where EVERY tool call blocked (and timed out →
            // denied) in `default` permission_mode:
            //   Auto  → allow instantly, no card (the common case).
            //   Block → deny instantly, no card (+ a toast + audit item).
            //   Gate  → fall through to the blocking card (current path).
            // Only Gate blocks; Auto/Block answer the hook immediately so
            // the agent never stalls on a safe or an obviously-dangerous
            // command. The same evaluator runs in the remote CLI as a
            // static fallback when this desktop is unreachable (66.D.1).
            if blocking && subkind == "pre-tool-use" {
                let policy_on = settings::load_from_disk()
                    .map(|s| s.hooks.policy_enabled)
                    .unwrap_or(true);
                if policy_on {
                    let tool_name = payload
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let bash_cmd = payload
                        .get("tool_input")
                        .and_then(|ti| ti.get("command"))
                        .and_then(|c| c.as_str());
                    let verdict = winmux_policy::evaluate(tool_name, bash_cmd);
                    crate::dlog(&format!(
                        "feed.push: policy tool={} decision={:?} matched={:?} req_id={}",
                        tool_name, verdict.decision, verdict.matched, req_id
                    ));
                    match verdict.decision {
                        winmux_policy::Decision::Auto => {
                            push_policy_audit(
                                state,
                                app,
                                &req_id,
                                "policy-auto",
                                &format!("✓ {tool_name}"),
                                &verdict.reason,
                                pane_id.clone(),
                                workspace_id.clone(),
                            );
                            return Ok(json!({
                                "request_id": req_id,
                                "decision": "allow",
                                "policy": "auto",
                            }));
                        }
                        winmux_policy::Decision::Block => {
                            push_policy_audit(
                                state,
                                app,
                                &req_id,
                                "policy-block",
                                &format!("⛔ Blocked: {tool_name}"),
                                &verdict.reason,
                                pane_id.clone(),
                                workspace_id.clone(),
                            );
                            // Phase 66 (KK): gate the block toast by the
                            // per-event toggle (default ON — security insight).
                            let bn = settings::load_from_disk().unwrap_or_default().notifications;
                            if bn.toast_enabled && bn.toast_block {
                                show_toast(&format!("⛔ Blocked: {tool_name}"), &verdict.reason);
                            }
                            return Ok(json!({
                                "request_id": req_id,
                                "decision": "deny",
                                "policy": "block",
                            }));
                        }
                        // Gate: fall through to the blocking card below.
                        winmux_policy::Decision::Gate => {}
                    }
                }
            }

            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);

            let item = FeedItem {
                request_id: req_id.clone(),
                kind: kind.clone(),
                subkind: subkind.clone(),
                pane_id,
                workspace_id,
                title: title.clone(),
                summary: summary.clone(),
                payload,
                state: if blocking {
                    FeedItemState::Pending
                } else {
                    FeedItemState::Passive
                },
                created_ms: now_ms,
                blocking,
            };

            // Register the item (and a oneshot for blocking ones).
            let oneshot_rx = if blocking {
                let (tx, rx) = tokio::sync::oneshot::channel::<String>();
                let mut store = state.feed.lock().unwrap();
                store.items.push_back(item.clone());
                while store.items.len() > FEED_MAX_ITEMS_LIMIT {
                    store.items.pop_front();
                }
                store.pending.insert(req_id.clone(), tx);
                Some(rx)
            } else {
                let mut store = state.feed.lock().unwrap();
                store.items.push_back(item.clone());
                while store.items.len() > FEED_MAX_ITEMS_LIMIT {
                    store.items.pop_front();
                }
                None
            };

            let _ = app.emit("feed:item-added", &item);
            // Phase 66 (KK): friendly, per-event-gated toast — never dump the
            // raw payload JSON, and stay silent for noisy lifecycle events
            // (session-start/end default OFF). `title`/`summary` from the CLI
            // are still used for the feed card; the TOAST text is humanized
            // from the payload here.
            {
                let s = settings::load_from_disk().unwrap_or_default();
                if hook_toast_enabled(&s.notifications, &subkind) {
                    // v0.4.4: `stop` fires at the END OF EVERY TURN, so a toast
                    // per turn would be noise when the user is already watching
                    // winmux (they see the feed card + sidebar highlight).
                    // Suppress the stop toast when the main window is focused;
                    // still toast when winmux is in the background ("your turn"
                    // while you're doing something else). SessionEnd (rare)
                    // always toasts.
                    let suppress_stop = subkind == "stop"
                        && app
                            .get_webview_window("main")
                            .and_then(|w| w.is_focused().ok())
                            .unwrap_or(false);
                    if !suppress_stop {
                        let (tt, tb) =
                            humanize_notification(&subkind, &item.payload, &s.i18n.language);
                        show_toast(&tt, &tb);
                    }
                }
            }

            // Phase 17: auto-summarize on Stop hook. The Claude Code
            // Stop hook arrives here as a feed.push with `subkind ==
            // "stop"` (passive). We fire-and-forget the summary task
            // — it reads settings, opens its own SSH exec channel,
            // saves a Note. Failures land in debug.log so the user
            // can diagnose without the hook itself failing.
            if subkind == "stop" {
                if let Some(ws) = item.workspace_id.clone() {
                    let pane = item.pane_id.clone();
                    let state_clone: crate::AppState = state.clone();
                    let app_clone = app.clone();
                    tokio::spawn(async move {
                        crate::claude_summary::auto_summarize_on_stop(
                            &state_clone,
                            &app_clone,
                            &ws,
                            pane.as_deref(),
                        )
                        .await;
                    });
                }
            }

            if let Some(rx) = oneshot_rx {
                let wait =
                    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await;
                let decision = match wait {
                    Ok(Ok(d)) => d,
                    Ok(Err(_)) => {
                        // sender dropped — mark as denied (e.g. app shutting down)
                        let _ = decide_feed(state, app, &req_id, "deny");
                        "deny".to_string()
                    }
                    Err(_) => {
                        let _ = decide_feed(state, app, &req_id, "timeout");
                        "timeout".to_string()
                    }
                };
                Ok(json!({ "request_id": req_id, "decision": decision }))
            } else {
                Ok(json!({ "request_id": req_id, "decision": "passive" }))
            }
        }

        // ─── Phase 7.B: notes ─────────────────────────────────────────────
        "note-add" => {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("missing text")?
                .to_string();
            let tag = params
                .get("tag")
                .and_then(|v| v.as_str())
                .map(String::from);
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            // pane_id: explicit param, else auto from caller's pane (set by spawn_ssh
            // env vars and propagated to RPC by future tunnel work — for now the
            // CLI fills it in). If pane_id is set and workspace_id isn't, look up.
            let mut pane_id = params
                .get("pane_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let workspace_id = match (workspace_id, &pane_id) {
                (Some(w), _) => Some(w),
                (None, Some(p)) => find_workspace_for_pane(&state.workspaces.lock().unwrap(), p),
                (None, None) => None,
            };
            if pane_id.as_deref() == Some("") {
                pane_id = None;
            }
            let n = notes::rpc_add(state, app, text, tag, workspace_id, pane_id)?;
            serde_json::to_value(&n).map_err(|e| e.to_string())
        }

        "note-list" => {
            let tag = params.get("tag").and_then(|v| v.as_str());
            let status_str = params.get("status").and_then(|v| v.as_str());
            let status = match status_str {
                Some("open") => Some(NoteStatus::Open),
                Some("done") => Some(NoteStatus::Done),
                _ => None,
            };
            let workspace_id = params.get("workspace_id").and_then(|v| v.as_str());
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|x| x as usize);
            let list = notes::list_filtered(state, tag, status, workspace_id, limit);
            serde_json::to_value(&list).map_err(|e| e.to_string())
        }

        "note-update" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .map(String::from);
            // tag: presence vs absence vs explicit null. JSON `null` becomes
            // `Some(Value::Null)`; a literal "" empty string clears too.
            let tag = if params.get("tag").is_some() {
                Some(
                    params
                        .get("tag")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                )
            } else {
                None
            };
            let status = match params.get("status").and_then(|v| v.as_str()) {
                Some("open") => Some(NoteStatus::Open),
                Some("done") => Some(NoteStatus::Done),
                _ => None,
            };
            let n = notes::rpc_update(state, app, &id, text, tag, status)?;
            serde_json::to_value(&n).map_err(|e| e.to_string())
        }

        "note-done" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            let n = notes::rpc_update(state, app, &id, None, None, Some(NoteStatus::Done))?;
            serde_json::to_value(&n).map_err(|e| e.to_string())
        }

        "note-delete" => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            notes::rpc_delete(state, app, &id)?;
            Ok(json!({ "ok": true }))
        }

        // ─── Phase 9.A: settings ─────────────────────────────────────────
        "settings.load" => {
            let s = state.settings.lock().unwrap().clone();
            serde_json::to_value(&s).map_err(|e| e.to_string())
        }
        "settings.save" => {
            // Accept either a full Settings object (overwrite) under
            // params.settings, or a partial JSON patch under params.patch.
            if let Some(full) = params.get("settings").cloned() {
                let parsed: settings::Settings =
                    serde_json::from_value(full).map_err(|e| format!("bad settings: {e}"))?;
                {
                    let mut s = state.settings.lock().unwrap();
                    *s = parsed;
                }
                let s = state.settings.lock().unwrap().clone();
                settings::save_to_disk_pub(&s)?;
                let _ = app.emit("settings:changed", &s);
                return serde_json::to_value(&s).map_err(|e| e.to_string());
            }
            if let Some(patch) = params.get("patch").cloned() {
                let s = settings::rpc_patch(state, app, patch)?;
                return serde_json::to_value(&s).map_err(|e| e.to_string());
            }
            Err("missing `settings` or `patch`".into())
        }
        "settings.set" => {
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or("missing key")?
                .to_string();
            let value = params
                .get("value")
                .ok_or("missing value")?
                .clone();
            // Coerce the value through string-or-direct so CLI users can pass
            // both `--value dracula` (raw string) and `--value true` / numeric.
            let value_str = match &value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let s = settings::rpc_set_path(state, app, &key, &value_str)?;
            serde_json::to_value(&s).map_err(|e| e.to_string())
        }
        "settings.preset" => {
            let id = params
                .get("preset")
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
                .ok_or("missing preset")?
                .to_string();
            let s = settings::rpc_apply_preset(state, app, &id)?;
            serde_json::to_value(&s).map_err(|e| e.to_string())
        }
        "settings.get-presets" => serde_json::to_value(&settings::list_presets())
            .map_err(|e| e.to_string()),

        // ─── Phase 9.B: update checker ───────────────────────────────────
        "updates.check" => Ok(updater::rpc_check_now(state, app).await),

        // ─── Phase 11.A: tmux persistence ───────────────────────────────
        "pane.persistence.get" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let pane_sessions = state.core.pane_sessions.lock().unwrap().clone();
            let sessions = state.core.sessions.lock().unwrap();
            let tmux = pane_sessions
                .get(&pane_id)
                .and_then(|sid| sessions.get(sid))
                .and_then(|s| match s {
                    crate::Session::Ssh(ssh) => ssh.tmux_session.clone(),
                    _ => None,
                });
            Ok(json!({ "pane_id": pane_id, "tmux_session": tmux }))
        }
        "pane.disconnect" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            // Phase 65 (bug FF): log remote-triggered disconnects so we can
            // tell if something on the server (a hook / CLI call) is what
            // closed the pane when Claude exited.
            crate::dlog(&format!("rpc pane.disconnect (remote-triggered) pane={pane_id}"));
            // Mirror what pane_disconnect Tauri command does, minus the
            // teardown_command path (which only matters for app shutdown).
            let sid = state.core.pane_sessions.lock().unwrap().remove(&pane_id);
            if let Some(sid) = sid {
                if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                    crate::kill_session_inner(&mut s);
                }
            }
            Ok(json!({ "ok": true, "pane_id": pane_id, "killed": false }))
        }
        "pane.kill-session" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            // Same flow as the Tauri command — open a fresh exec channel,
            // run `tmux kill-session`, then close.
            let sid_opt = state.core.pane_sessions.lock().unwrap().get(&pane_id).cloned();
            if let Some(sid) = sid_opt {
                let (handle_arc, tmux_name) = {
                    let sessions = state.core.sessions.lock().unwrap();
                    match sessions.get(&sid) {
                        Some(crate::Session::Ssh(s)) => {
                            (Some(s.handle.clone()), s.tmux_session.clone())
                        }
                        _ => (None, None),
                    }
                };
                if let (Some(handle), Some(name)) = (handle_arc, tmux_name) {
                    let cmd = format!(
                        "tmux kill-session -t {} 2>&1 || true",
                        crate::shell_quote(&name)
                    );
                    if let Ok(mut ch) = handle.channel_open_session().await {
                        let _ = ch.exec(true, cmd.as_bytes()).await;
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(800),
                            async {
                                while let Some(msg) = ch.wait().await {
                                    use russh::ChannelMsg;
                                    if matches!(
                                        msg,
                                        ChannelMsg::ExitStatus { .. }
                                            | ChannelMsg::Eof
                                            | ChannelMsg::Close
                                    ) {
                                        break;
                                    }
                                }
                            },
                        )
                        .await;
                        let _ = ch.close().await;
                    }
                }
                let sid = state.core.pane_sessions.lock().unwrap().remove(&pane_id);
                if let Some(sid) = sid {
                    if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                        crate::kill_session_inner(&mut s);
                    }
                }
            }
            Ok(json!({ "ok": true, "pane_id": pane_id, "killed": true }))
        }
        // ─── Phase 12.B: smart connect + claude session browser ─────────
        "claude.sessions.list" => {
            let workspace_id = params
                .get("workspace_id")
                .or_else(|| params.get("workspace"))
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            // Re-dispatch through the Tauri command's underlying logic.
            // We can't call the #[tauri::command] fn directly from here
            // (its signature requires State<'_, AppState>), but the work
            // is identical: look up an SSH handle, run the same script.
            // Easiest: replicate via crate::pane_list_claude_sessions_impl
            // — but to keep changes small, we duplicate the workspace SSH
            // lookup here. NOTE: pane_list_claude_sessions(_impl)? helper
            // could be factored later.
            let handle_opt = {
                let sessions = state.core.sessions.lock().unwrap();
                sessions
                    .iter()
                    .find_map(|(_sid, sess)| match sess {
                        crate::Session::Ssh(s) if s.workspace_id == workspace_id => {
                            Some(s.handle.clone())
                        }
                        _ => None,
                    })
            };
            let lim = limit.unwrap_or(30).min(200);
            let script = format!(
                "find \"$HOME/.claude/projects\" -maxdepth 4 -name '*.jsonl' \
                 -printf '%T@\\t%p\\n' 2>/dev/null | sort -rn | head -{} | \
                 while IFS=$'\\t' read -r mt path; do \
                   first_user=$(head -100 \"$path\" 2>/dev/null | grep -m1 -E '\"role\"\\s*:\\s*\"user\"' | head -c 600); \
                   last_asst=$(tail -200 \"$path\" 2>/dev/null | grep -E '\"role\"\\s*:\\s*\"assistant\"' | tail -1 | head -c 600); \
                   printf '%s\\t%s\\t%s\\t%s\\n' \"$mt\" \"$path\" \"$first_user\" \"$last_asst\"; \
                 done",
                lim
            );
            if let Some(handle) = handle_opt {
                let mut ch = handle
                    .channel_open_session()
                    .await
                    .map_err(|e| format!("channel_open: {e}"))?;
                ch.exec(true, script.as_bytes())
                    .await
                    .map_err(|e| format!("exec: {e}"))?;
                let mut out_bytes = Vec::new();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                    while let Some(msg) = ch.wait().await {
                        match msg {
                            russh::ChannelMsg::Data { ref data } => out_bytes.extend_from_slice(data),
                            russh::ChannelMsg::Eof
                            | russh::ChannelMsg::Close
                            | russh::ChannelMsg::ExitStatus { .. } => break,
                            _ => {}
                        }
                    }
                })
                .await;
                let _ = ch.close().await;
                let raw = String::from_utf8_lossy(&out_bytes).to_string();
                let mut out = serde_json::Map::new();
                let mut arr: Vec<Value> = Vec::new();
                for line in raw.lines() {
                    let parts: Vec<&str> = line.splitn(4, '\t').collect();
                    if parts.len() < 2 {
                        continue;
                    }
                    let mtime = parts[0]
                        .split('.')
                        .next()
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    let path = parts[1].to_string();
                    let session_id = std::path::Path::new(&path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let project = std::path::Path::new(&path)
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    arr.push(json!({
                        "session_id": session_id,
                        "project_path": project,
                        "jsonl_path": path,
                        "mtime_unix": mtime,
                    }));
                }
                out.insert("sessions".into(), Value::Array(arr));
                Ok(Value::Object(out))
            } else {
                // Local fallback handled by the Tauri command — RPC only
                // makes sense for SSH workspaces (the CLI runs on the
                // remote side anyway). Return empty.
                Ok(json!({ "sessions": [] }))
            }
        }
        "pane.persistence.list" => {
            let pane_sessions = state.core.pane_sessions.lock().unwrap().clone();
            let sessions = state.core.sessions.lock().unwrap();
            let mut out = serde_json::Map::new();
            for (pane, sid) in pane_sessions {
                if let Some(crate::Session::Ssh(ssh)) = sessions.get(&sid) {
                    if let Some(name) = &ssh.tmux_session {
                        out.insert(pane, Value::String(name.clone()));
                    }
                }
            }
            Ok(Value::Object(out))
        }

        "feed.decide" => {
            let req_id = params
                .get("request_id")
                .and_then(|v| v.as_str())
                .ok_or("missing request_id")?
                .to_string();
            let decision = params
                .get("decision")
                .and_then(|v| v.as_str())
                .ok_or("missing decision")?
                .to_string();
            decide_feed(state, app, &req_id, &decision)?;
            Ok(json!({ "ok": true }))
        }

        // Phase 8 fix v3: emergency reset for a workspace whose layout has been
        // corrupted (typically by the recent autosave loop). Replaces the layout
        // with a single fresh terminal pane using the inferred connection.
        "reset-layout" => {
            let id = params
                .get("id")
                .or_else(|| params.get("workspace_id"))
                .and_then(|v| v.as_str())
                .ok_or("missing id")?
                .to_string();
            {
                let mut file = state.workspaces.lock().unwrap();
                let ws = file
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or_else(|| format!("no workspace {id}"))?;
                let inferred = ws
                    .layout
                    .as_ref()
                    .and_then(crate::first_terminal_connection_pub)
                    .or_else(|| ws.connection.clone())
                    .unwrap_or(Connection::Local { shell: None });
                ws.layout = Some(LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: PaneKind::Terminal,
                    connection: Some(inferred),
                    browser: None,
                    title: None,
                    annotation: None,
                    color: None,
                    emoji: None,
                    help_topic: None,
                    diff_source: None,
                    smart_bidi: None,
                });
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "workspace_id": id }))
        }

        // Phase 8.E: introspection. Pure reads of AppState + on-disk debug.log.
        "dev.get-state" => Ok(build_dev_state(state, 50, 50)),

        // Phase 48-C: /doctor diagnostic snapshot. Same payload as the
        // tauri `doctor` command, reusable from the bundled CLI via
        // `winmux doctor` so support tickets can be dumped at the
        // command line.
        "doctor" => Ok(crate::build_doctor_snapshot(state)),

        "dev.console-tail" => {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            let entries = dev::console_tail(&state.console_buffer, limit);
            Ok(serde_json::to_value(&entries).map_err(|e| e.to_string())?)
        }

        "dev.debug-log-tail" => {
            let limit = params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            let dir = config_dir_pub().map_err(|e| e.to_string())?;
            let lines = dev::debug_log_tail(&dir.join("debug.log"), limit);
            Ok(json!(lines))
        }

        "dev.report-bug" => {
            let description = params
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or("missing description")?
                .to_string();
            let repro_steps = params
                .get("repro_steps")
                .and_then(|v| v.as_str())
                .map(String::from);
            let snapshot = build_dev_state(state, 200, 200);
            let body = json!({
                "description": description,
                "repro_steps": repro_steps,
                "captured_at_unix": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                "state": snapshot,
            });
            let dir = config_dir_pub().map_err(|e| e.to_string())?;
            let path = dev::write_bug_report(&dir, &body)?;
            Ok(json!({ "ok": true, "path": path.to_string_lossy() }))
        }

        // Phase 46: the remote port-watcher reports listening ports
        // here. We DETECT only — no automatic forward, no FeedItem.
        // The UI shows the detected port; the user clicks to open
        // the tunnel via `forward_port_start`. The workspace's
        // `auto_port_forward` flag still gates detection itself
        // (watcher off = nothing reaches here in the first place,
        // but we double-check so toggling off mid-stream stops the
        // backend recording stale ports).
        "port.opened" => {
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            let port = params
                .get("port")
                .and_then(|v| v.as_u64())
                .ok_or("missing port")? as u16;
            let addr = params
                .get("addr")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();
            let family = params
                .get("family")
                .and_then(|v| v.as_str())
                .unwrap_or("v4")
                .to_string();
            // Phase 39: never report winmux's own reverse-tunnel port.
            let is_internal = {
                let m = state.core.internal_reverse_tunnel_remote_ports.lock().unwrap();
                m.get(&workspace_id).map(|s| s.contains(&port)).unwrap_or(false)
            };
            if is_internal {
                return Ok(json!({ "ok": true, "skipped": "winmux internal port" }));
            }
            let enabled = {
                let file = state.workspaces.lock().unwrap();
                file.workspaces
                    .iter()
                    .find(|w| w.id == workspace_id)
                    .map(|w| w.auto_port_forward)
                    .unwrap_or(false)
            };
            if !enabled {
                return Ok(json!({ "ok": true, "skipped": "detection off" }));
            }
            // Record + notify FE. No forward is opened.
            {
                let mut m = state.core.detected_ports.lock().unwrap();
                m.entry(workspace_id.clone())
                    .or_default()
                    .insert(port, (addr.clone(), family.clone()));
            }
            let _ = app.emit(
                "port-detected",
                json!({
                    "workspace_id": workspace_id,
                    "addr": addr,
                    "remote_port": port,
                    "family": family,
                }),
            );
            Ok(json!({ "ok": true, "detected": true }))
        }

        "port.closed" => {
            let workspace_id = params
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .ok_or("missing workspace_id")?
                .to_string();
            let port = params
                .get("port")
                .and_then(|v| v.as_u64())
                .ok_or("missing port")? as u16;
            // Drop from detected set + tell the FE.
            let was_detected = {
                let mut m = state.core.detected_ports.lock().unwrap();
                m.get_mut(&workspace_id)
                    .map(|ports| ports.remove(&port).is_some())
                    .unwrap_or(false)
            };
            if was_detected {
                let _ = app.emit(
                    "port-undetected",
                    json!({
                        "workspace_id": workspace_id,
                        "remote_port": port,
                    }),
                );
            }
            // If this port was actually forwarded, tear that down too.
            // close_one_forward is a no-op if no entry exists.
            crate::close_one_forward(state, app, &workspace_id, port);
            Ok(json!({ "ok": true }))
        }

        other => Err(format!("unknown method: {other}")),
    }
}

// Phase 8.E: build the dev.get-state Value. Uses module-level statics for
// version/git_hash/build_time which are baked in at compile time by build.rs.
fn build_dev_state(state: &AppState, log_tail_n: usize, console_tail_n: usize) -> Value {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const GIT_HASH: &str = match option_env!("WINMUX_GIT_HASH") {
        Some(h) => h,
        None => "unknown",
    };
    let build_time: u64 = option_env!("WINMUX_BUILD_TIME")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let dir = config_dir_pub().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Snapshot under short locks.
    let workspaces_clone = state.workspaces.lock().unwrap().clone();

    let pane_to_workspace: std::collections::HashMap<String, String> = {
        let mut m = std::collections::HashMap::new();
        for ws in &workspaces_clone.workspaces {
            if let Some(layout) = &ws.layout {
                let mut panes = Vec::new();
                collect_panes(layout, &mut panes);
                for p in panes {
                    m.insert(p, ws.id.clone());
                }
            }
        }
        m
    };

    let pane_kind_map: std::collections::HashMap<String, PaneKind> = {
        let mut m = std::collections::HashMap::new();
        for ws in &workspaces_clone.workspaces {
            if let Some(layout) = &ws.layout {
                fold_pane_kinds(layout, &mut m);
            }
        }
        m
    };

    let sessions_summary: Vec<dev::SessionSummary> = {
        let pane_sessions = state.core.pane_sessions.lock().unwrap().clone();
        let sessions = state.core.sessions.lock().unwrap();
        pane_sessions
            .iter()
            .filter_map(|(pane_id, sid)| {
                let s = sessions.get(sid)?;
                let (kind, conn_type, ws_id) = match s {
                    Session::Local(_) => (
                        "terminal",
                        Some("local".to_string()),
                        pane_to_workspace.get(pane_id).cloned(),
                    ),
                    Session::Ssh(ssh) => (
                        "terminal",
                        Some("ssh".to_string()),
                        Some(ssh.workspace_id.clone()),
                    ),
                };
                let _ = pane_kind_map.get(pane_id); // could be browser; sessions are for terminal only
                Some(dev::SessionSummary {
                    pane_id: pane_id.clone(),
                    kind: kind.to_string(),
                    connection_type: conn_type,
                    workspace_id: ws_id,
                })
            })
            .collect()
    };

    let forwards_summary: Vec<dev::ForwardSummary> = {
        let m = state.core.forwards.lock().unwrap();
        m.iter()
            .map(|((ws_id, remote_port), entry)| dev::ForwardSummary {
                workspace_id: ws_id.clone(),
                remote_port: *remote_port,
                local_port: entry.local_port,
            })
            .collect()
    };

    let feed_counts: dev::FeedCounts = {
        let store = state.feed.lock().unwrap();
        let mut c = dev::FeedCounts::default();
        for it in &store.items {
            match it.state {
                FeedItemState::Pending | FeedItemState::Passive => c.open += 1,
                _ => c.done += 1,
            }
            *c.by_kind.entry(it.kind.clone()).or_insert(0) += 1;
        }
        c
    };

    let notes_counts: dev::NotesCounts = {
        let nf = state.notes.lock().unwrap();
        let mut c = dev::NotesCounts::default();
        for n in &nf.notes {
            match n.status {
                crate::notes::NoteStatus::Open => c.open += 1,
                crate::notes::NoteStatus::Done => c.done += 1,
            }
            if let Some(t) = &n.tag {
                *c.by_tag.entry(t.clone()).or_insert(0) += 1;
            }
        }
        c
    };

    let log_tail = dev::debug_log_tail(&dir.join("debug.log"), log_tail_n);
    let console_tail = dev::console_tail(&state.console_buffer, console_tail_n);

    // Suppress unused warning for connection-type silencer in the SSH branch.
    let _ = Connection::Local { shell: None };

    dev::build_state_value(
        VERSION,
        GIT_HASH,
        build_time,
        &dir,
        &workspaces_clone,
        sessions_summary,
        forwards_summary,
        feed_counts,
        notes_counts,
        log_tail,
        console_tail,
    )
}

// Helper for dev.get-state — collect every pane's kind into a map keyed by pane_id.
fn fold_pane_kinds(node: &LayoutNode, out: &mut std::collections::HashMap<String, PaneKind>) {
    match node {
        LayoutNode::Pane {
            pane_id, pane_kind, ..
        } => {
            out.insert(pane_id.clone(), *pane_kind);
        }
        LayoutNode::Split { first, second, .. } => {
            fold_pane_kinds(first, out);
            fold_pane_kinds(second, out);
        }
    }
    let _ = collect_panes_with_kind; // keep symbol live for other call sites
}

#[cfg(test)]
mod tests {
    use super::make_listener;

    // Phase 39.C: the regression was max_instances(255) panicking at
    // construction, which killed the pipe server. Confirm the listener
    // now builds (254 is within the tokio limit) and that make_listener
    // returns Ok rather than unwinding. Needs a tokio runtime because
    // ServerOptions::create registers the pipe with the IO reactor.
    #[tokio::test]
    async fn make_listener_builds_without_panic() {
        // Unique name per run so concurrent test invocations don't clash.
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!(r"\\.\pipe\winmux-test-39c-{n}");
        let listener = make_listener(&name);
        assert!(
            listener.is_ok(),
            "make_listener should build at 254: {:?}",
            listener.err()
        );
        // Opening a SECOND instance of the same name must also succeed
        // (proves max_instances > 1 took effect).
        let second = make_listener(&name);
        assert!(second.is_ok(), "second instance: {:?}", second.err());
    }

    // Phase 44: a small pool of listeners serves multiple concurrent
    // clients without ERROR_PIPE_NOT_AVAILABLE (231). The pre-39.A and
    // Phase 39.A code both kept only ONE listener in accept state, so a
    // burst of N>1 simultaneous client opens would race and the loser
    // would get 231. With POOL_SIZE listeners simultaneously in accept
    // state, up to POOL_SIZE concurrent opens succeed immediately.
    #[tokio::test]
    async fn pool_serves_concurrent_clients_without_busy() {
        const POOL_SIZE: usize = 8;
        const CONCURRENT_CLIENTS: usize = 4;
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!(r"\\.\pipe\winmux-test-44-pool-{n}");

        // Mini-pool mirroring spawn_listener_pool's accept loop, but
        // without the full handler (a no-op task takes the connection so
        // the slot can recreate its listener immediately).
        for _slot in 0..POOL_SIZE {
            let name = name.clone();
            tokio::spawn(async move {
                loop {
                    let listener = match super::make_listener(&name) {
                        Ok(l) => l,
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            continue;
                        }
                    };
                    if listener.connect().await.is_ok() {
                        tokio::spawn(async move {
                            // Hold briefly so the client's open is fully
                            // acknowledged, then drop.
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            drop(listener);
                        });
                    }
                }
            });
        }

        // Let the pool come up.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Fire CONCURRENT_CLIENTS pipe opens at once; all must succeed.
        let handles: Vec<_> = (0..CONCURRENT_CLIENTS)
            .map(|i| {
                let name = name.clone();
                tokio::spawn(async move {
                    let r = tokio::net::windows::named_pipe::ClientOptions::new().open(&name);
                    (i, r)
                })
            })
            .collect();

        for h in handles {
            let (i, r) = h.await.expect("client task panicked");
            assert!(
                r.is_ok(),
                "client {i} expected Ok, got {:?} (231 = pool exhausted)",
                r.as_ref().err().map(|e| e.raw_os_error())
            );
        }
    }

    // v0.3.1 pipe-instance-leak regression: 500 SEQUENTIAL one-shot clients
    // (simulating 500 Claude hook calls) must all succeed. The pre-fix handler
    // looped forever (never dropping its NamedPipeServer), so each connection
    // permanently consumed one of the 254 max_instances and the pipe was
    // exhausted after ~254 — exactly the bug that disconnected sessions. With
    // the one-shot handler (read one line, reply, DROP), instances are freed
    // and 500+ connections cycle cleanly.
    #[tokio::test]
    async fn pool_survives_500_sequential_oneshot_clients() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        const POOL_SIZE: usize = 8;
        const CLIENTS: usize = 500;
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!(r"\\.\pipe\winmux-test-031-leak-{n}");

        for _ in 0..POOL_SIZE {
            let name = name.clone();
            tokio::spawn(async move {
                loop {
                    let listener = match super::make_listener(&name) {
                        Ok(l) => l,
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            continue;
                        }
                    };
                    if listener.connect().await.is_ok() {
                        // One-shot handler mirroring the fixed handle_client:
                        // read one line, reply, then drop → free the instance.
                        tokio::spawn(async move {
                            let (rh, mut wh) = tokio::io::split(listener);
                            let mut br = tokio::io::BufReader::new(rh);
                            let mut line = String::new();
                            let _ = br.read_line(&mut line).await;
                            let _ = wh.write_all(b"OK\n").await;
                            let _ = wh.flush().await;
                            // br + wh drop here → NamedPipeServer freed.
                        });
                    }
                }
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        for i in 0..CLIENTS {
            // Open (retry only on transient 231, capped so a real leak FAILS
            // the test instead of hanging).
            let mut client = {
                let mut attempt = 0;
                loop {
                    match tokio::net::windows::named_pipe::ClientOptions::new().open(&name) {
                        Ok(c) => break c,
                        Err(e) if e.raw_os_error() == Some(231) && attempt < 200 => {
                            attempt += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                        Err(e) => panic!(
                            "client {i} failed to open after {attempt} retries: {:?} \
                             (231 sustained = pipe-instance leak)",
                            e.raw_os_error()
                        ),
                    }
                }
            };
            client.write_all(b"REQ\n").await.unwrap();
            let mut buf = [0u8; 3];
            client
                .read_exact(&mut buf)
                .await
                .unwrap_or_else(|e| panic!("client {i} read failed: {e}"));
            assert_eq!(&buf, b"OK\n", "client {i} got wrong reply");
        }
    }
}
