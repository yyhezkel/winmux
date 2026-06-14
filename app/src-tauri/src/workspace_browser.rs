//! Phase 53 (rebased): workspace-level Browser singleton.
//!
//! Replaces the Phase 53.A per-pane Browser surface. Each workspace
//! owns AT MOST ONE child Webview attached to the main window. When
//! the user opens the floating Browser window for workspace `w_X`, the
//! frontend calls `workspace_browser_show(w_X, url, x, y, w, h)`:
//!
//! - If no Webview exists for `w_X` yet, we spawn one via
//!   `Window::add_child(WebviewBuilder, ...)` with
//!   `--user-data-dir="<config_dir>/browser-sessions/<w_X>/"` so
//!   cookies + localStorage + cache survive restarts and don't
//!   cross-contaminate between workspaces.
//! - If one already exists, we reposition/resize it and call
//!   `.show()` (the floating-window pattern can hide a workspace's
//!   browser when the user closes the floating panel, then bring it
//!   back when they reopen it without losing the page state).
//!
//! Z-order: native Webview always paints above HTML, so any modal
//! opening in the SolidJS layer broadcasts `workspace_browser_hide`
//! for the active workspace and `workspace_browser_show` again on
//! close.
//!
//! Pinned tauri =2.10.3 with `features = ["unstable"]` — the
//! `Window::add_child(WebviewBuilder, ...)` API still lives behind
//! the unstable gate in this version. See CLAUDE.md "Pinned deps".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::webview::WebviewBuilder;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, State, Url, Webview, WebviewUrl,
};

use crate::{config_dir, dlog, AppState};

/// Map of `workspace_id -> Webview`. Exactly one entry per workspace
/// that has opened its Browser at least once this session. Cleared
/// by `workspace_browser_close` (user closed the floating window
/// explicitly) and by `cleanup_workspace_sessions` (user deleted the
/// workspace).
pub(crate) type WorkspaceBrowserMap = Arc<Mutex<HashMap<String, Webview>>>;

fn webview_label(workspace_id: &str) -> String {
    // Tauri webview labels are constrained to [a-zA-Z-/:_].
    // workspace_id is `w_<hex>` which is alnum+underscore — safe.
    format!("workspace-browser-{workspace_id}")
}

/// Resolve the per-workspace browser session directory. Created on
/// first spawn; never auto-deleted (sessions outlive any modal that
/// was open over them). `workspace_delete` is responsible for
/// `rm -rf` on the workspace's dir — see `cleanup_workspace_sessions`
/// below.
fn workspace_session_dir(workspace_id: &str) -> Result<std::path::PathBuf, String> {
    let dir = config_dir()?
        .join("browser-sessions")
        .join(sanitize_for_path(workspace_id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir session dir: {e}"))?;
    Ok(dir)
}

/// Workspace IDs are `w_<hex>` so this is defence-in-depth — any
/// `..`, `/`, `\` would already be rejected by the ID format.
fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build the `--user-data-dir=...` arg that WebView2 picks up. The
/// path is wrapped in double-quotes because Windows paths contain
/// backslashes that WebView2's arg parser otherwise mishandles.
fn user_data_dir_arg(workspace_id: &str) -> Result<String, String> {
    let dir = workspace_session_dir(workspace_id)?;
    Ok(format!(
        "--user-data-dir=\"{}\"",
        dir.to_string_lossy().replace('"', "")
    ))
}

/// Spawn (if absent) + reposition + show. Frontend calls this every
/// time the floating Browser window mounts or its rect changes.
///
/// `url` is only consulted when we actually spawn a new Webview —
/// re-shows leave the existing Webview's URL alone (Browser window
/// preserves page state across hide/show cycles).
#[tauri::command]
pub(crate) async fn workspace_browser_show(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    url: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    // Fast path: the Webview already exists. Reposition + .show().
    {
        let map = state.workspace_browsers.lock().unwrap();
        if let Some(webview) = map.get(&workspace_id).cloned() {
            drop(map);
            webview
                .set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("set_position: {e}"))?;
            webview
                .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
                .map_err(|e| format!("set_size: {e}"))?;
            webview.show().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    // Slow path: spawn a new child Webview. Phase 62.A (item D):
    // serialize creation across ALL workspaces — WebView2 dislikes
    // concurrent environment creation and returns 0x8007139F
    // (ERROR_INVALID_STATE). The guard is held across the whole creation
    // (including the retry backoff) so two rapid opens can't race.
    let _create_guard = state.browser_create_lock.lock().await;

    // Re-check under the creation lock: another call may have created
    // the webview while we were waiting on the lock. Without this, two
    // concurrent show() calls for the same workspace would both miss the
    // fast path above and both call add_child → a duplicate-label /
    // same-user-data-dir failure (a second ERROR_INVALID_STATE source).
    {
        let map = state.workspace_browsers.lock().unwrap();
        if let Some(webview) = map.get(&workspace_id).cloned() {
            drop(map);
            webview
                .set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("set_position: {e}"))?;
            webview
                .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
                .map_err(|e| format!("set_size: {e}"))?;
            webview.show().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }

    let main_window = app
        .get_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let parsed_url: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let label = webview_label(&workspace_id);
    let user_data_arg = user_data_dir_arg(&workspace_id)?;

    // Retry the transient WebView2 ERROR_INVALID_STATE a couple of times
    // with a short backoff (the builder is consumed by add_child, so we
    // rebuild it each attempt). A clean failure is surfaced to the FE
    // only after all attempts are exhausted.
    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    let mut created = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed_url.clone()))
            .additional_browser_args(&user_data_arg);
        match main_window.add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(w.max(1.0), h.max(1.0)),
        ) {
            Ok(wv) => {
                dlog(&format!(
                    "[workspace_browser_show] add_child ws={} ok (attempt {}/{})",
                    workspace_id, attempt, MAX_ATTEMPTS
                ));
                created = Some(wv);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                dlog(&format!(
                    "[workspace_browser_show] add_child ws={} attempt {}/{} FAILED: {}",
                    workspace_id, attempt, MAX_ATTEMPTS, last_err
                ));
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
            }
        }
    }
    let webview =
        created.ok_or_else(|| format!("add_child failed after {MAX_ATTEMPTS} attempts: {last_err}"))?;

    state
        .workspace_browsers
        .lock()
        .unwrap()
        .insert(workspace_id.clone(), webview);

    dlog(&format!(
        "[workspace_browser_show] spawned ws={} url={} rect=({:.0},{:.0},{:.0},{:.0})",
        workspace_id, url, x, y, w, h
    ));
    Ok(())
}

/// Hide the workspace's Browser Webview if one exists. No-op if not
/// (modal effect may broadcast hide for every workspace; spurious
/// hides for never-opened workspaces are silent).
#[tauri::command]
pub(crate) async fn workspace_browser_hide(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned();
    if let Some(w) = webview {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_navigate(
    state: State<'_, AppState>,
    workspace_id: String,
    url: String,
) -> Result<(), String> {
    let parsed: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    webview.navigate(parsed).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_eval(
    state: State<'_, AppState>,
    workspace_id: String,
    js: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    webview.eval(js).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_close(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .remove(&workspace_id);
    if let Some(w) = webview {
        let _ = w.close();
        dlog(&format!(
            "[workspace_browser_close] dropped webview ws={}",
            workspace_id
        ));
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn workspace_browser_resize(
    state: State<'_, AppState>,
    workspace_id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for workspace {workspace_id}"))?;
    webview
        .set_position(LogicalPosition::new(x, y))
        .map_err(|e| format!("set_position: {e}"))?;
    webview
        .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
        .map_err(|e| format!("set_size: {e}"))?;
    Ok(())
}

/// `workspace_delete` hooks here to wipe the per-workspace session
/// dir (the user explicitly deleted the workspace — they don't want
/// cookies surviving). Best-effort; errors are logged not raised.
pub(crate) fn cleanup_workspace_sessions(workspace_id: &str) {
    let Ok(base) = config_dir() else {
        return;
    };
    let dir = base
        .join("browser-sessions")
        .join(sanitize_for_path(workspace_id));
    if !dir.exists() {
        return;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => dlog(&format!(
            "[workspace_browser] cleaned sessions dir for ws={} at {}",
            workspace_id,
            dir.display()
        )),
        Err(e) => dlog(&format!(
            "[workspace_browser] FAILED to clean sessions dir {}: {}",
            dir.display(),
            e
        )),
    }
}
