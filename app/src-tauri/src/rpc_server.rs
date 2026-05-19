use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::notes::{self, NoteStatus};
use crate::dev;
use crate::settings;
use crate::updater;
use crate::{
    collect_panes, collect_panes_with_kind, config_dir_pub, decide_feed, find_browser_state,
    find_workspace_for_pane, iframe_cmd_inner, new_pane_id, new_workspace_id,
    next_browser_request_id, persist, resolve_browser_url, split_pane_in, update_browser_pane,
    update_pane_in, write_to_session, AppState, Connection, CreateInput, EnvVar, FeedItem,
    FeedItemState, LayoutNode, NotificationItem, PaneKind, Session, SplitDirection, Workspace,
    NOTIF_COUNTER,
};

const FEED_MAX_ITEMS_LIMIT: usize = 50;

pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\winmux-{}", user)
}

pub async fn run(state: AppState, app: AppHandle) {
    let name = pipe_name();
    tracing::info!("rpc: listening on {}", name);
    loop {
        let server = match ServerOptions::new()
            .pipe_mode(tokio::net::windows::named_pipe::PipeMode::Byte)
            .first_pipe_instance(false)
            .max_instances(8)
            .create(&name)
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("rpc: create pipe failed: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        if let Err(e) = server.connect().await {
            tracing::error!("rpc: connect failed: {e}");
            continue;
        }
        let state2 = state.clone();
        let app2 = app.clone();
        tokio::spawn(handle_client(server, state2, app2));
    }
}

async fn handle_client(stream: NamedPipeServer, state: AppState, app: AppHandle) {
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let req: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                let _ = writer.write_all(format!("{resp}\n").as_bytes()).await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let result = dispatch(&method, params, &state, &app).await;
        let resp = match result {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": e }
            }),
        };
        let _ = writer.write_all(format!("{resp}\n").as_bytes()).await;
        let _ = writer.flush().await;
    }
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

async fn dispatch(
    method: &str,
    params: Value,
    state: &AppState,
    app: &AppHandle,
) -> Result<Value, String> {
    match method {
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
                cwd: input.cwd,
                connection: None,
                layout: Some(LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: PaneKind::Terminal,
                    connection: Some(input.connection),
                    browser: None,
                    chat: None,
                    title: None,
                    annotation: None,
                }),
                setup_command: input.setup_command,
                teardown_command: input.teardown_command,
                env: input.env.unwrap_or_default(),
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
                if let Some(sid) = state.pane_sessions.lock().unwrap().remove(pane_id) {
                    if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
                        crate::kill_session_inner(&mut s);
                    }
                }
            }
            // Phase 8.B: tear down any port forwards for the workspace.
            crate::close_workspace_forwards(&state.forwards, &id);
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
            let sid = state
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
            let sid = state
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
            let item = NotificationItem {
                id: NOTIF_COUNTER.fetch_add(1, Ordering::Relaxed),
                title: title.clone(),
                body: body.clone(),
                workspace_id,
                timestamp_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
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
                let layout_fallback = state
                    .workspaces
                    .lock()
                    .unwrap()
                    .workspaces
                    .iter()
                    .find(|w| w.id == workspace_id)
                    .and_then(|w| w.layout.as_ref().and_then(crate::first_terminal_connection_pub));
                layout_fallback
                    .or_else(|| crate::live_ssh_connection_for_workspace_pub(state, &workspace_id))
            } else {
                None
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        let (new_layout, _) =
                            split_pane_in(layout, &pane_id, direction, kind, url, fallback_conn);
                        ws.layout = Some(new_layout);
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "workspace_id": workspace_id }))
        }

        // Phase 8.A: browser pane navigation. All three resolve workspace_id from
        // pane_id so agents on a remote can call them with just $WINMUX_PANE_ID.
        "pane.browser.navigate" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("missing url")?
                .to_string();
            if url.is_empty() {
                return Err("empty url".into());
            }
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        let url_clone = url.clone();
                        ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                            if !b.url.is_empty() && b.url != url_clone {
                                b.history.push(b.url.clone());
                                if b.history.len() > 50 {
                                    let drop = b.history.len() - 50;
                                    b.history.drain(0..drop);
                                }
                            }
                            b.url = url_clone.clone();
                        }));
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "url": url }))
        }

        "pane.browser.go-back" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                            if let Some(prev) = b.history.pop() {
                                b.url = prev;
                            }
                        }));
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true }))
        }

        // Phase 8.C: read the current URL from the pane's persisted browser state.
        "pane.browser.url" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let bs = find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
            Ok(json!({ "url": bs.url, "home_url": bs.home_url }))
        }

        // Phase 8.C: read the navigation history.
        "pane.browser.history" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let bs = find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
            Ok(json!({ "history": bs.history, "current": bs.url }))
        }

        // Phase 8.C: block until the iframe fires onload (or timeout). Returns
        // the URL the frontend reported. Default timeout 10s.
        "pane.browser.wait" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000);
            // Reject straight away if the pane isn't a browser pane (avoids
            // hanging forever on a stale id).
            let bs = find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
            // Phase 8.C fix: short-circuit when the iframe has already fired
            // its `load` for the current url. Without this, a wait issued
            // after the page is already loaded would block until the next
            // navigation (or timeout).
            if let Some(loaded) = &bs.last_loaded_url {
                if loaded == &bs.url {
                    return Ok(json!({ "url": bs.url, "already_loaded": true }));
                }
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            state
                .browser_load_waiters
                .lock()
                .unwrap()
                .entry(pane_id.clone())
                .or_default()
                .push(tx);
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                rx,
            )
            .await;
            match result {
                Ok(Ok(url)) => Ok(json!({ "url": url })),
                Ok(Err(_)) => Err("waiter dropped".into()),
                Err(_) => {
                    // Best-effort: leave any other waiters in place.
                    Err(format!("timeout after {timeout_ms} ms"))
                }
            }
        }

        // Phase 8.C: ask the frontend to evaluate JS on the iframe. Same-origin
        // only — cross-origin returns a clean error suggesting Phase 8.D.
        "pane.browser.eval" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let expression = params
                .get("expression")
                .or_else(|| params.get("expr"))
                .and_then(|v| v.as_str())
                .ok_or("missing expression")?
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
            let request_id = next_browser_request_id();
            let (tx, rx) = tokio::sync::oneshot::channel();
            state
                .browser_pending
                .lock()
                .unwrap()
                .insert(request_id.clone(), tx);
            let _ = app.emit(
                "browser:request",
                json!({
                    "request_id": request_id,
                    "kind": "eval",
                    "pane_id": pane_id,
                    "expression": expression,
                }),
            );
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
            match result {
                Ok(Ok(Ok(v))) => Ok(v),
                Ok(Ok(Err(e))) => Err(e),
                Ok(Err(_)) => Err("response channel closed".into()),
                Err(_) => {
                    state.browser_pending.lock().unwrap().remove(&request_id);
                    Err(format!("eval timeout after {timeout_ms} ms"))
                }
            }
        }

        // Phase 8.C: take a screenshot of the pane via the frontend (html2canvas).
        // Optional `output_path` writes the PNG to disk; otherwise returns base64.
        // NOTE: cross-origin iframes render as blanks under html2canvas; OS-level
        // capture lands in 8.D with WebView2 native panes.
        "pane.browser.screenshot" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let output_path = params
                .get("output_path")
                .or_else(|| params.get("output"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(15_000);
            find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
            let request_id = next_browser_request_id();
            let (tx, rx) = tokio::sync::oneshot::channel();
            state
                .browser_pending
                .lock()
                .unwrap()
                .insert(request_id.clone(), tx);
            let _ = app.emit(
                "browser:request",
                json!({
                    "request_id": request_id,
                    "kind": "screenshot",
                    "pane_id": pane_id,
                }),
            );
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
            let dataurl_value = match result {
                Ok(Ok(Ok(v))) => v,
                Ok(Ok(Err(e))) => return Err(e),
                Ok(Err(_)) => return Err("response channel closed".into()),
                Err(_) => {
                    state.browser_pending.lock().unwrap().remove(&request_id);
                    return Err(format!("screenshot timeout after {timeout_ms} ms"));
                }
            };
            let dataurl = dataurl_value
                .as_str()
                .ok_or("frontend response was not a data URL string")?;
            let b64 = dataurl
                .strip_prefix("data:image/png;base64,")
                .ok_or("frontend did not return data:image/png;base64,...")?;
            if let Some(path) = output_path {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("base64 decode: {e}"))?;
                std::fs::write(&path, &bytes).map_err(|e| format!("write {path}: {e}"))?;
                Ok(json!({ "ok": true, "path": path, "bytes": bytes.len() }))
            } else {
                Ok(json!({ "data_url": dataurl }))
            }
        }

        // Phase 8.B: resolve a URL through the workspace's port-forward map.
        // For agents on remote that just want the rewritten URL string.
        "pane.browser.resolve-url" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("missing url")?
                .to_string();
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            let resolved = resolve_browser_url(state, &workspace_id, &pane_id, &url).await?;
            Ok(json!({ "url": resolved }))
        }

        "pane.browser.go-home" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let workspace_id = {
                let file = state.workspaces.lock().unwrap();
                find_workspace_for_pane(&file, &pane_id)
                    .ok_or_else(|| format!("no pane {pane_id}"))?
            };
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                            if let Some(home) = b.home_url.clone() {
                                if !b.url.is_empty() && b.url != home {
                                    b.history.push(b.url.clone());
                                    if b.history.len() > 50 {
                                        let drop = b.history.len() - 50;
                                        b.history.drain(0..drop);
                                    }
                                }
                                b.url = home;
                            }
                        }));
                    }
                }
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true }))
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
            show_toast(&title, &summary);

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
            let pane_sessions = state.pane_sessions.lock().unwrap().clone();
            let sessions = state.sessions.lock().unwrap();
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
            // Mirror what pane_disconnect Tauri command does, minus the
            // teardown_command path (which only matters for app shutdown).
            let sid = state.pane_sessions.lock().unwrap().remove(&pane_id);
            if let Some(sid) = sid {
                if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
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
            let sid_opt = state.pane_sessions.lock().unwrap().get(&pane_id).cloned();
            if let Some(sid) = sid_opt {
                let (handle_arc, tmux_name) = {
                    let sessions = state.sessions.lock().unwrap();
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
                let sid = state.pane_sessions.lock().unwrap().remove(&pane_id);
                if let Some(sid) = sid {
                    if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
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
                let sessions = state.sessions.lock().unwrap();
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
            let pane_sessions = state.pane_sessions.lock().unwrap().clone();
            let sessions = state.sessions.lock().unwrap();
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
                    chat: None,
                    title: None,
                    annotation: None,
                });
            }
            persist(state)?;
            let _ = app.emit("workspaces:changed", ());
            Ok(json!({ "ok": true, "workspace_id": id }))
        }

        // ─── Phase 8.F.1: iframe automation via postMessage bridge ─────────
        "pane.browser.iframe.click" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let selector = params
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or("missing selector")?
                .to_string();
            let button = params
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left")
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            iframe_cmd_inner(
                state,
                app,
                &pane_id,
                "click",
                json!({ "selector": selector, "button": button }),
                timeout_ms,
            )
            .await
        }

        "pane.browser.iframe.type" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let selector = params
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or("missing selector")?
                .to_string();
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("missing text")?
                .to_string();
            let clear_first = params
                .get("clear_first")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            iframe_cmd_inner(
                state,
                app,
                &pane_id,
                "type",
                json!({ "selector": selector, "text": text, "clear_first": clear_first }),
                timeout_ms,
            )
            .await
        }

        "pane.browser.iframe.find" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            // Pass the whole params blob through as the find query, but
            // strip our own meta keys so the bridge only sees match filters.
            let mut q = serde_json::Map::new();
            for (k, v) in params.as_object().into_iter().flat_map(|m| m.iter()) {
                match k.as_str() {
                    "pane_id" | "pane" | "timeout_ms" => continue,
                    _ => {
                        q.insert(k.clone(), v.clone());
                    }
                }
            }
            iframe_cmd_inner(state, app, &pane_id, "find", json!(q), timeout_ms).await
        }

        "pane.browser.iframe.wait-for" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let inner_timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            // Strip pane_id meta; pass the rest as the criteria object so
            // the bridge sees selector / text / role / label / testid /
            // urlContains / state / timeout_ms verbatim.
            let mut q = serde_json::Map::new();
            for (k, v) in params.as_object().into_iter().flat_map(|m| m.iter()) {
                match k.as_str() {
                    "pane_id" | "pane" => continue,
                    _ => {
                        q.insert(k.clone(), v.clone());
                    }
                }
            }
            // Outer (backend) timeout = bridge timeout + 2 s buffer so the
            // bridge gets a chance to return its own structured timeout error
            // rather than us preempting it with the generic IPC timeout.
            iframe_cmd_inner(
                state,
                app,
                &pane_id,
                "wait-for",
                json!(q),
                inner_timeout_ms + 2_000,
            )
            .await
        }

        "pane.browser.iframe.snapshot" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000);
            let max_depth = params.get("maxDepth").or_else(|| params.get("max_depth"));
            let text_only = params.get("textOnly").or_else(|| params.get("text_only"));
            let opts = json!({
                "maxDepth": max_depth.cloned().unwrap_or(json!(50)),
                "textOnly": text_only.cloned().unwrap_or(json!(false)),
            });
            iframe_cmd_inner(state, app, &pane_id, "snapshot", opts, timeout_ms).await
        }

        "pane.browser.iframe.eval" => {
            let pane_id = params
                .get("pane_id")
                .or_else(|| params.get("pane"))
                .and_then(|v| v.as_str())
                .ok_or("missing pane_id")?
                .to_string();
            let expression = params
                .get("expression")
                .or_else(|| params.get("expr"))
                .and_then(|v| v.as_str())
                .ok_or("missing expression")?
                .to_string();
            let timeout_ms = params
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(5_000);
            iframe_cmd_inner(
                state,
                app,
                &pane_id,
                "eval",
                json!({ "expression": expression }),
                timeout_ms,
            )
            .await
        }

        // Phase 8.E: introspection. Pure reads of AppState + on-disk debug.log.
        "dev.get-state" => Ok(build_dev_state(state, 50, 50)),

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
        let pane_sessions = state.pane_sessions.lock().unwrap().clone();
        let sessions = state.sessions.lock().unwrap();
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
        let m = state.forwards.lock().unwrap();
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
