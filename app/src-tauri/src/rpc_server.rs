use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::{
    collect_panes, new_pane_id, new_workspace_id, persist, write_to_session, AppState,
    CreateInput, LayoutNode, NotificationItem, Workspace, NOTIF_COUNTER,
};

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
                    connection: input.connection,
                }),
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

        other => Err(format!("unknown method: {other}")),
    }
}
