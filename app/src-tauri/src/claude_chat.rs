//! Phase 22: backend for the ClaudeChat pane.
//!
//! 22.A ships an echo-only stub: `claude_chat_send` appends the user
//! message to the pane's persisted chat history, immediately appends a
//! mocked assistant reply of the form `Echo: <user message>`, and
//! returns the updated state. This lets the entire UI shell — bubbles,
//! input bar, scroll, persistence, layout integration — be exercised
//! and committed before the streaming-CLI work in 22.B.
//!
//! 22.B will:
//!   - replace the echo with a real `claude -p --output-format=stream-json …`
//!     exec, streamed token-by-token over `claude:chat:token` events,
//!   - capture the session_id Claude returns on the first call and
//!     pass it as `--resume <id>` on subsequent calls,
//!   - honor `model` from settings or per-pane override,
//!   - route through the workspace's existing SSH session for SSH
//!     workspaces, and through a local `claude.exe` lookup otherwise.
//!
//! Storage: the chat history lives on the `LayoutNode::Pane.chat`
//! field as `ClaudeChatState`, persisted in `workspaces.json` next to
//! everything else. The frontend reads it back via the regular
//! `workspaces_load` and `workspaces:changed` event flow — no
//! separate persistence path.

use tauri::{AppHandle, Emitter, State};

use crate::{
    dlog, persist, update_chat_pane, AppState, ChatMessage, ChatRole, ClaudeChatState,
    MessageStatus, WorkspacesFile,
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

/// Phase 22.A: send a chat message and get an echoed assistant reply.
/// Appends two `ChatMessage` entries (the user's, then the assistant's)
/// to the pane's `ClaudeChatState.messages`, persists the workspaces
/// file, and returns the full updated file so the frontend can refresh
/// its layout view. 22.B will turn this into a streaming call.
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
    let assistant_msg = ChatMessage {
        id: new_message_id(),
        role: ChatRole::Assistant,
        // 22.A stub. 22.B replaces this with streaming tokens from the
        // real claude CLI exec.
        content: format!("Echo: {trimmed}"),
        timestamp: iso_now(),
        status: MessageStatus::Done,
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
    Ok(state.workspaces.lock().unwrap().clone())
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

/// Phase 22.A: set the per-pane model override. None means "let
/// `claude` pick whatever's in the user's CLI config".
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
            // Keep this for completeness; not used to short-circuit anything.
            let _ = c.session_id.is_some();
        }));
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 22.A: read a pane's chat state. Mostly redundant with
/// `workspaces_load` but lets the frontend re-fetch a single pane
/// without parsing the whole layout tree.
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

fn find_chat_in(node: &crate::LayoutNode, target: &str) -> Option<ClaudeChatState> {
    match node {
        crate::LayoutNode::Pane {
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
        crate::LayoutNode::Split { first, second, .. } => {
            find_chat_in(first, target).or_else(|| find_chat_in(second, target))
        }
    }
}
