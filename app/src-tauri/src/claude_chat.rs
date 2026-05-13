//! Phase 22: backend for the ClaudeChat pane.
//!
//! **22.B**: real `claude` CLI integration with streaming.
//! `claude_chat_send` runs the user's prompt through
//! `claude -p --output-format=stream-json --verbose [--resume <session>]`
//! on the workspace's existing SSH session (or a local `claude` if the
//! workspace is purely local), parses the line-delimited JSON output,
//! and emits `claude:chat:token` / `claude:chat:done` / `claude:chat:error`
//! Tauri events as the response streams in. The assistant message is
//! also persisted into workspaces.json once complete so a restart
//! keeps the chat history.
//!
//! Storage layout, frontend wiring, and the supporting clear/model
//! commands are unchanged from 22.A. The only behavioral difference is
//! `claude_chat_send` now talks to a real CLI instead of echoing.

use std::sync::Arc;

use russh::client::Handle as SshHandle;
use russh::ChannelMsg;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    dlog, persist, update_chat_pane, AppState, ChatMessage, ChatRole, ClaudeChatState, Connection,
    LayoutNode, MessageStatus, Session, SshClient, WorkspacesFile,
};

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn new_message_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("m_{:x}", t)
}

fn pick_ssh_handle(state: &AppState, workspace_id: &str) -> Option<Arc<SshHandle<SshClient>>> {
    let sessions = state.sessions.lock().ok()?;
    sessions.values().find_map(|s| match s {
        Session::Ssh(ssh) if ssh.workspace_id == workspace_id => Some(ssh.handle.clone()),
        _ => None,
    })
}

fn workspace_is_local(state: &AppState, workspace_id: &str) -> bool {
    let file = state.workspaces.lock().ok();
    let Some(file) = file else { return false };
    let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
    let Some(ws) = ws else { return false };
    let layout = match &ws.layout {
        Some(l) => l,
        None => return false,
    };
    has_local_terminal(layout)
}

fn has_local_terminal(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane { connection, .. } => {
            matches!(connection, Some(Connection::Local { .. }))
        }
        LayoutNode::Split { first, second, .. } => {
            has_local_terminal(first) || has_local_terminal(second)
        }
    }
}

fn pane_session_and_model(
    state: &AppState,
    workspace_id: &str,
    pane_id: &str,
) -> (Option<String>, Option<String>) {
    let file = match state.workspaces.lock() {
        Ok(f) => f,
        Err(_) => return (None, None),
    };
    let ws = match file.workspaces.iter().find(|w| w.id == workspace_id) {
        Some(w) => w,
        None => return (None, None),
    };
    let Some(layout) = &ws.layout else {
        return (None, None);
    };
    fn find(node: &LayoutNode, target: &str) -> Option<ClaudeChatState> {
        match node {
            LayoutNode::Pane { pane_id, chat, .. } if pane_id == target => chat.clone(),
            LayoutNode::Pane { .. } => None,
            LayoutNode::Split { first, second, .. } => {
                find(first, target).or_else(|| find(second, target))
            }
        }
    }
    match find(layout, pane_id) {
        Some(c) => (c.session_id, c.model),
        None => (None, None),
    }
}

/// Append an in-place delta to an in-flight assistant message — used
/// while the streaming task is running. The final state is also
/// persisted once the message is complete.
fn append_to_message(
    state: &AppState,
    workspace_id: &str,
    pane_id: &str,
    message_id: &str,
    delta: &str,
    new_status: Option<MessageStatus>,
    capture_session_id: Option<&str>,
) {
    let mut file = match state.workspaces.lock() {
        Ok(f) => f,
        Err(_) => return,
    };
    let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
        return;
    };
    let Some(layout) = ws.layout.take() else {
        return;
    };
    ws.layout = Some(update_chat_pane(layout, pane_id, &mut |c| {
        if let Some(sid) = capture_session_id {
            if !sid.is_empty() && c.session_id.as_deref() != Some(sid) {
                c.session_id = Some(sid.to_string());
            }
        }
        if let Some(msg) = c.messages.iter_mut().find(|m| m.id == message_id) {
            msg.content.push_str(delta);
            if let Some(s) = new_status {
                msg.status = s;
            }
        }
    }));
}

/// Bash-single-quote-safe escape for inclusion in `claude -p '<...>'`.
/// Matches claude_summary::bash_squote so the two stay in lockstep.
fn bash_squote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Phase 22.B-fix: wrap a script in `bash -lc '<script>'` so the
/// remote/local shell sources the user's `~/.bash_profile` /
/// `~/.profile` / `~/.bashrc`. SSH execs run a non-interactive
/// non-login shell by default — that DOES NOT load rc files and so
/// any PATH extension users put there (e.g. `export PATH=$HOME/.claude/local:$PATH`)
/// is invisible to us. `bash -lc` flips both flags on.
fn wrap_login(script: &str) -> String {
    format!("bash -lc {}", bash_squote(script))
}

/// Build the bare bash pipeline (no `bash -lc` wrapper) that runs
/// `claude -p` with the right flags. The caller wraps for SSH /
/// local-unix as appropriate; on Windows local we run it through
/// PowerShell directly (claude.exe lives on the global Windows PATH
/// when installed via npm-global, so login-shell sourcing is moot).
///
/// `claude_path` is the absolute path discovered by
/// `detect_claude_path` (cached per workspace). When detection
/// failed we fall back to bare `claude` and let login-shell PATH
/// catch it — better than refusing to try.
fn build_claude_pipeline(
    claude_path: &str,
    prompt: &str,
    session_id: Option<&str>,
    model: Option<&str>,
) -> String {
    let mut flags = String::from(" -p --output-format=stream-json --verbose --dangerously-skip-permissions");
    if let Some(sid) = session_id {
        if !sid.is_empty() {
            flags.push_str(&format!(" --resume {}", bash_squote(sid)));
        }
    }
    if let Some(m) = model {
        if !m.is_empty() {
            flags.push_str(&format!(" --model {}", bash_squote(m)));
        }
    }
    format!(
        "printf %s {prompt} | {bin}{flags}",
        prompt = bash_squote(prompt),
        bin = bash_squote(claude_path),
        flags = flags,
    )
}

const CLAUDE_DETECT_SCRIPT: &str = "\
command -v claude 2>/dev/null && exit 0; \
for p in \
  $HOME/.claude/local/claude \
  $HOME/.local/bin/claude \
  /usr/local/bin/claude \
  /opt/homebrew/bin/claude \
  $HOME/.nvm/versions/node/*/bin/claude \
  $HOME/.fnm/aliases/default/bin/claude; do \
  if [ -x \"$p\" ]; then echo \"$p\"; exit 0; fi; \
done; \
exit 127";

fn cache_key(workspace_id: &str, scope: &str) -> String {
    format!("{workspace_id}:{scope}")
}

fn cached_claude_path(state: &AppState, workspace_id: &str, scope: &str) -> Option<String> {
    let map = state.claude_paths.lock().ok()?;
    map.get(&cache_key(workspace_id, scope)).cloned()
}

fn remember_claude_path(state: &AppState, workspace_id: &str, scope: &str, path: &str) {
    if let Ok(mut map) = state.claude_paths.lock() {
        map.insert(cache_key(workspace_id, scope), path.to_string());
    }
}

/// One-shot SSH exec that captures stdout. Mirrors
/// `claude_summary::ssh_exec` so the two stay parallel.
async fn ssh_exec_capture(
    handle: &SshHandle<SshClient>,
    cmd: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    Ok(String::from_utf8_lossy(&stdout).to_string())
}

async fn detect_claude_path_ssh(
    handle: &SshHandle<SshClient>,
) -> Option<String> {
    let out = ssh_exec_capture(handle, &wrap_login(CLAUDE_DETECT_SCRIPT), 8)
        .await
        .ok()?;
    let path = out.lines().find(|l| !l.trim().is_empty())?.trim().to_string();
    if path.is_empty() || path.starts_with("ERROR") {
        None
    } else {
        Some(path)
    }
}

async fn detect_claude_path_local() -> Option<String> {
    use tokio::process::Command;
    let output = if cfg!(target_os = "windows") {
        // On Windows there's no `bash -lc` (unless WSL); rely on
        // standard `where claude` which checks PATH (including
        // npm-global bin if it's there).
        Command::new("where").arg("claude").output().await.ok()?
    } else {
        Command::new("bash")
            .args(["-lc", CLAUDE_DETECT_SCRIPT])
            .output()
            .await
            .ok()?
    };
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text.lines().find(|l| !l.trim().is_empty())?.trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

async fn resolve_claude_path_ssh(
    state: &AppState,
    workspace_id: &str,
    handle: &SshHandle<SshClient>,
) -> String {
    if let Some(cached) = cached_claude_path(state, workspace_id, "ssh") {
        return cached;
    }
    match detect_claude_path_ssh(handle).await {
        Some(p) => {
            dlog(&format!(
                "claude_chat: detected claude at {p} (ws={workspace_id} ssh)"
            ));
            remember_claude_path(state, workspace_id, "ssh", &p);
            p
        }
        None => {
            dlog(&format!(
                "claude_chat: claude path detection failed for ws={workspace_id} ssh — falling back to bare `claude`"
            ));
            "claude".to_string()
        }
    }
}

async fn resolve_claude_path_local(state: &AppState, workspace_id: &str) -> String {
    if let Some(cached) = cached_claude_path(state, workspace_id, "local") {
        return cached;
    }
    match detect_claude_path_local().await {
        Some(p) => {
            dlog(&format!(
                "claude_chat: detected claude at {p} (ws={workspace_id} local)"
            ));
            remember_claude_path(state, workspace_id, "local", &p);
            p
        }
        None => {
            dlog(&format!(
                "claude_chat: claude path detection failed for ws={workspace_id} local — falling back to bare `claude`"
            ));
            "claude".to_string()
        }
    }
}

/// Try to pull a text delta and session_id out of one stream-json
/// line. Returns (text_to_append, captured_session_id). Best-effort —
/// anything we can't parse is silently dropped (claude's stream-json
/// includes lots of metadata events we don't care about).
fn parse_stream_line(line: &str) -> (Option<String>, Option<String>) {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let session_id = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    // Two shapes we care about:
    //   { "type": "assistant", "message": { "content": [ { "type": "text", "text": "..." } ] } }
    //   { "type": "result", "result": "final text" }
    let text = if ty == "assistant" {
        v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                let mut buf = String::new();
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            buf.push_str(t);
                        }
                    }
                }
                if buf.is_empty() {
                    None
                } else {
                    Some(buf)
                }
            })
    } else {
        None
    };
    (text, session_id)
}

#[derive(Clone, serde::Serialize)]
struct TokenEvent {
    workspace_id: String,
    pane_id: String,
    message_id: String,
    delta: String,
    session_id: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct DoneEvent {
    workspace_id: String,
    pane_id: String,
    message_id: String,
    session_id: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct ErrorEvent {
    workspace_id: String,
    pane_id: String,
    message_id: String,
    error: String,
}

/// Run one streaming exec over an existing SSH session. Reads stdout
/// chunks, splits on `\n`, parses stream-json, accumulates text
/// deltas, and emits `claude:chat:token` events for each chunk. The
/// caller is responsible for persisting the assistant's final content
/// + status.
async fn stream_over_ssh(
    handle: &SshHandle<SshClient>,
    cmd: &str,
    workspace_id: &str,
    pane_id: &str,
    message_id: &str,
    state: &AppState,
    app: &AppHandle,
) -> Result<(Option<String>, bool), String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd)
        .await
        .map_err(|e| format!("exec: {e}"))?;

    let mut leftover = Vec::<u8>::new();
    let mut captured_session: Option<String> = None;
    let mut had_text = false;
    // Sane cap. Claude responses for normal prompts finish well under
    // 5 minutes; longer is almost always a stuck connection.
    let res = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => {
                    leftover.extend_from_slice(data);
                    // Drain whole lines.
                    while let Some(pos) = leftover.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = leftover.drain(..=pos).collect();
                        let text = String::from_utf8_lossy(&line).into_owned();
                        let (delta, sid) = parse_stream_line(&text);
                        if let Some(sid) = sid {
                            if captured_session.is_none() {
                                captured_session = Some(sid);
                            }
                        }
                        if let Some(d) = delta {
                            had_text = true;
                            append_to_message(
                                state,
                                workspace_id,
                                pane_id,
                                message_id,
                                &d,
                                Some(MessageStatus::Sending),
                                captured_session.as_deref(),
                            );
                            let _ = app.emit(
                                "claude:chat:token",
                                TokenEvent {
                                    workspace_id: workspace_id.to_string(),
                                    pane_id: pane_id.to_string(),
                                    message_id: message_id.to_string(),
                                    delta: d,
                                    session_id: captured_session.clone(),
                                },
                            );
                        }
                    }
                }
                ChannelMsg::ExtendedData { ref data, .. } => {
                    // stderr — surfaced if we never got real output.
                    leftover.extend_from_slice(data);
                }
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    if res.is_err() {
        return Err("claude stream timed out after 5 minutes".into());
    }
    Ok((captured_session, had_text))
}

/// Local-workspace fallback: spawn `claude` via tokio Command and
/// stream stdout the same way.
///
/// `pipeline` is the bare `printf %s 'msg' | <path> -p …` script.
/// On non-Windows we run it via `bash -lc` so the user's
/// `~/.bash_profile` / `~/.bashrc` is sourced (claude installed
/// only on the interactive PATH would otherwise be invisible).
/// On Windows we hand it to PowerShell directly — the Windows PATH
/// is set in registry, not shell rc, so login-shell sourcing is
/// moot. (PowerShell needs `printf` not to mean its alias; we use
/// `&` and call ops cautiously.)
async fn stream_locally(
    pipeline: &str,
    workspace_id: &str,
    pane_id: &str,
    message_id: &str,
    state: &AppState,
    app: &AppHandle,
) -> Result<(Option<String>, bool), String> {
    use tokio::io::AsyncBufReadExt;
    use tokio::process::Command;

    let mut child = if cfg!(target_os = "windows") {
        // PowerShell doesn't have `printf %s …`; rewrite to use
        // Set-Content + cat-like pipe. For now, use cmd.exe so we
        // can write a sh-style pipe verbatim — claude on Windows
        // typically lives at %APPDATA%\npm\claude.cmd or via
        // Node's bin shim, both of which work from cmd.exe.
        Command::new("cmd")
            .args(["/c", pipeline])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn cmd: {e}"))?
    } else {
        Command::new("bash")
            .args(["-lc", pipeline])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn bash -lc: {e}"))?
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no stdout pipe".to_string())?;
    let mut reader = tokio::io::BufReader::new(stdout).lines();
    let mut captured_session: Option<String> = None;
    let mut had_text = false;
    let res = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        while let Ok(Some(line)) = reader.next_line().await {
            let (delta, sid) = parse_stream_line(&line);
            if let Some(sid) = sid {
                if captured_session.is_none() {
                    captured_session = Some(sid);
                }
            }
            if let Some(d) = delta {
                had_text = true;
                append_to_message(
                    state,
                    workspace_id,
                    pane_id,
                    message_id,
                    &d,
                    Some(MessageStatus::Sending),
                    captured_session.as_deref(),
                );
                let _ = app.emit(
                    "claude:chat:token",
                    TokenEvent {
                        workspace_id: workspace_id.to_string(),
                        pane_id: pane_id.to_string(),
                        message_id: message_id.to_string(),
                        delta: d,
                        session_id: captured_session.clone(),
                    },
                );
            }
        }
    })
    .await;
    let _ = child.kill().await;
    if res.is_err() {
        return Err("claude stream timed out after 5 minutes".into());
    }
    Ok((captured_session, had_text))
}

/// Phase 22.B: send a chat message and stream the assistant reply
/// from `claude -p --output-format=stream-json`. Returns the post-
/// append state (user message + empty assistant placeholder) so the
/// frontend renders the bubbles immediately; the streaming task fills
/// in the assistant content via `claude:chat:token` events.
#[tauri::command]
pub(crate) async fn claude_chat_send(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    content: String,
) -> Result<WorkspacesFile, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("empty message".into());
    }
    dlog(&format!(
        "claude_chat_send: ws={workspace_id} pane={pane_id} len={}",
        trimmed.len()
    ));

    let user_msg = ChatMessage {
        id: new_message_id(),
        role: ChatRole::User,
        content: trimmed.to_string(),
        timestamp: iso_now(),
        status: MessageStatus::Done,
    };
    let assistant_id = new_message_id();
    let assistant_msg = ChatMessage {
        id: assistant_id.clone(),
        role: ChatRole::Assistant,
        content: String::new(),
        timestamp: iso_now(),
        status: MessageStatus::Sending,
    };

    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .take()
            .ok_or_else(|| "workspace has no layout".to_string())?;
        ws.layout = Some(update_chat_pane(layout, &pane_id, &mut |c| {
            c.messages.push(user_msg.clone());
            c.messages.push(assistant_msg.clone());
        }));
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());

    let snapshot = state.workspaces.lock().unwrap().clone();

    // Fire the streaming exec in a background task and return
    // immediately. Frontend listens for claude:chat:token/done/error.
    let app_clone = app.clone();
    let workspace_id_clone = workspace_id.clone();
    let pane_id_clone = pane_id.clone();
    let assistant_id_clone = assistant_id.clone();
    let prompt = trimmed.to_string();
    tokio::spawn(async move {
        run_stream_task(
            app_clone,
            workspace_id_clone,
            pane_id_clone,
            assistant_id_clone,
            prompt,
        )
        .await;
    });

    Ok(snapshot)
}

async fn run_stream_task(
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    message_id: String,
    prompt: String,
) {
    let state = app.state::<AppState>();
    let (resume_id, model) = pane_session_and_model(&state, &workspace_id, &pane_id);

    let result: Result<(Option<String>, bool), String> =
        if let Some(handle) = pick_ssh_handle(&state, &workspace_id) {
            // Detect (or use cached) absolute claude path for this workspace.
            let claude_path = resolve_claude_path_ssh(&state, &workspace_id, &handle).await;
            let pipeline =
                build_claude_pipeline(&claude_path, &prompt, resume_id.as_deref(), model.as_deref());
            // Wrap once in `bash -lc` so the remote shell loads
            // ~/.bash_profile / ~/.bashrc — claude itself may shell
            // out to node / helpers that expect the user's full PATH.
            let cmd = wrap_login(&pipeline);
            stream_over_ssh(
                &handle,
                &cmd,
                &workspace_id,
                &pane_id,
                &message_id,
                &state,
                &app,
            )
            .await
        } else if workspace_is_local(&state, &workspace_id) {
            let claude_path = resolve_claude_path_local(&state, &workspace_id).await;
            let pipeline =
                build_claude_pipeline(&claude_path, &prompt, resume_id.as_deref(), model.as_deref());
            stream_locally(&pipeline, &workspace_id, &pane_id, &message_id, &state, &app).await
        } else {
            Err("no active SSH session for this workspace — connect a terminal pane first".into())
        };

    match result {
        Ok((session_id, had_text)) => {
            // Mark message Done. If we never got any text, that's
            // typically a "claude not installed" or auth error —
            // surface it as an error bubble.
            let final_status = if had_text {
                MessageStatus::Done
            } else {
                MessageStatus::Error
            };
            // We pass an empty delta — just to flip the status.
            append_to_message(
                &state,
                &workspace_id,
                &pane_id,
                &message_id,
                if had_text {
                    ""
                } else {
                    "(no response — is claude installed and authenticated?)"
                },
                Some(final_status),
                session_id.as_deref(),
            );
            let _ = persist(&state);
            let _ = app.emit("workspaces:changed", ());
            let _ = app.emit(
                "claude:chat:done",
                DoneEvent {
                    workspace_id,
                    pane_id,
                    message_id,
                    session_id,
                },
            );
        }
        Err(e) => {
            dlog(&format!("claude_chat_send stream task error: {e}"));
            append_to_message(
                &state,
                &workspace_id,
                &pane_id,
                &message_id,
                &format!("\n\n[error: {e}]"),
                Some(MessageStatus::Error),
                None,
            );
            let _ = persist(&state);
            let _ = app.emit("workspaces:changed", ());
            let _ = app.emit(
                "claude:chat:error",
                ErrorEvent {
                    workspace_id,
                    pane_id,
                    message_id,
                    error: e,
                },
            );
        }
    }
}

/// Phase 22.A: clear the chat history for a pane. Useful for "Start
/// over" buttons in the UI; preserves session_id and model so a fresh
/// chat resumes against the same Claude session if 22.B is wired up.
#[tauri::command]
pub(crate) async fn claude_chat_clear(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    drop_session_id: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .take()
            .ok_or_else(|| "workspace has no layout".to_string())?;
        ws.layout = Some(update_chat_pane(layout, &pane_id, &mut |c| {
            c.messages.clear();
            if drop_session_id {
                c.session_id = None;
            }
        }));
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Set the per-pane model override. None means "let `claude` pick
/// whatever's in the user's CLI config".
#[tauri::command]
pub(crate) async fn claude_chat_set_model(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    model: Option<String>,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .take()
            .ok_or_else(|| "workspace has no layout".to_string())?;
        ws.layout = Some(update_chat_pane(layout, &pane_id, &mut |c| {
            c.model = model
                .as_ref()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty());
        }));
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Read a pane's chat state without parsing the whole layout tree.
#[tauri::command]
pub(crate) fn claude_chat_get_state(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
) -> Result<Option<ClaudeChatState>, String> {
    let file = state.workspaces.lock().unwrap();
    let ws = file
        .workspaces
        .iter()
        .find(|w| w.id == workspace_id)
        .ok_or_else(|| format!("no workspace {workspace_id}"))?;
    let layout = match &ws.layout {
        Some(l) => l,
        None => return Ok(None),
    };
    Ok(find_chat_in(layout, &pane_id))
}

fn find_chat_in(node: &LayoutNode, target: &str) -> Option<ClaudeChatState> {
    match node {
        LayoutNode::Pane {
            pane_id,
            chat,
            ..
        } => {
            if pane_id == target {
                chat.clone()
            } else {
                None
            }
        }
        LayoutNode::Split { first, second, .. } => {
            find_chat_in(first, target).or_else(|| find_chat_in(second, target))
        }
    }
}
