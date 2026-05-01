use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::notes::{self, NoteStatus};
use crate::{
    collect_panes, decide_feed, find_browser_state, find_workspace_for_pane, new_pane_id,
    new_workspace_id, next_browser_request_id, persist, resolve_browser_url, split_pane_in,
    update_browser_pane, update_pane_in, write_to_session, AppState, CreateInput, EnvVar, FeedItem,
    FeedItemState, LayoutNode, NotificationItem, PaneKind, SplitDirection, Workspace,
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
            {
                let mut file = state.workspaces.lock().unwrap();
                if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if let Some(layout) = ws.layout.take() {
                        let (new_layout, _) =
                            split_pane_in(layout, &pane_id, direction, kind, url);
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
            find_browser_state(state, &pane_id)
                .ok_or_else(|| format!("no browser pane {pane_id}"))?;
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

        other => Err(format!("unknown method: {other}")),
    }
}
