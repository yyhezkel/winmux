//! Phase 53 (#4.8 / 48-F): browser-per-workspace.
//!
//! Replaces the iframe-based browser pane with a per-workspace Tauri 2
//! child Webview. Each Webview gets `--user-data-dir` pointing at
//! `<config_dir>/browser-sessions/<workspace_id>/`, so cookies +
//! localStorage + cache survive restarts and don't cross-contaminate
//! between workspaces.
//!
//! Pinned tauri =2.10.3 with `features = ["unstable"]` (the
//! `Window::add_child(WebviewBuilder, ...)` API still lives behind the
//! unstable gate in this version — see CLAUDE.md "Pinned deps").
//!
//! The frontend owns layout. It tells us where the placeholder div is
//! and we mirror that rect with `set_position` + `set_size` on every
//! ResizeObserver tick. Z-order: native Webview always paints above
//! HTML, so any modal opening in the SolidJS layer calls
//! `browser_pane_hide` first; closing the modal flips it back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::webview::WebviewBuilder;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, State, Url, Webview, WebviewUrl,
};

use crate::{config_dir, dlog, AppState};

/// Map of `pane_id -> Webview`. Each browser pane owns exactly one
/// child Webview attached to the main window. The Webview's
/// user-data-dir is keyed by workspace_id (not pane_id), so two
/// browser panes in the same workspace share a session — same as a
/// single browser with two tabs would.
pub(crate) type BrowserWebviewMap = Arc<Mutex<HashMap<String, Webview>>>;

fn webview_label(pane_id: &str) -> String {
    // Tauri webview labels are constrained to [a-zA-Z-/:_].
    // pane_id is `p_<hex>_<hex>` which is alnum+underscore — safe.
    format!("browser-{pane_id}")
}

/// Resolve the per-workspace browser session directory. Created on
/// first spawn; never auto-deleted (sessions outlive the modal that
/// was open over them). Workspace delete is responsible for `rm -rf`
/// on its own dir — see `cleanup_workspace_sessions` below.
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

#[tauri::command]
pub(crate) async fn browser_pane_spawn(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    url: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    // Defensive: if we already have a Webview for this pane_id, close
    // it before spawning a fresh one. Avoids a "label already in use"
    // error if the frontend remounts (HMR, error boundary recovery,
    // double-mount during dev).
    {
        let mut map = state.browser_webviews.lock().unwrap();
        if let Some(old) = map.remove(&pane_id) {
            let _ = old.close();
            dlog(&format!(
                "[browser_pane_spawn] replacing existing webview pane_id={}",
                pane_id
            ));
        }
    }

    let main_window = app
        .get_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    let parsed_url: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let label = webview_label(&pane_id);
    let user_data_arg = user_data_dir_arg(&workspace_id)?;

    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed_url))
        .additional_browser_args(&user_data_arg);

    let webview = main_window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(w.max(1.0), h.max(1.0)),
        )
        .map_err(|e| format!("add_child: {e}"))?;

    state
        .browser_webviews
        .lock()
        .unwrap()
        .insert(pane_id.clone(), webview);

    dlog(&format!(
        "[browser_pane_spawn] ws={} pane={} url={} rect=({:.0},{:.0},{:.0},{:.0})",
        workspace_id, pane_id, url, x, y, w, h
    ));
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_navigate(
    state: State<'_, AppState>,
    pane_id: String,
    url: String,
) -> Result<(), String> {
    let parsed: Url = url
        .parse()
        .map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let webview = state
        .browser_webviews
        .lock()
        .unwrap()
        .get(&pane_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for pane {pane_id}"))?;
    webview.navigate(parsed).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_close(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let webview = state.browser_webviews.lock().unwrap().remove(&pane_id);
    if let Some(w) = webview {
        let _ = w.close();
        dlog(&format!("[browser_pane_close] pane={}", pane_id));
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_resize(
    state: State<'_, AppState>,
    pane_id: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let webview = state
        .browser_webviews
        .lock()
        .unwrap()
        .get(&pane_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for pane {pane_id}"))?;
    webview
        .set_position(LogicalPosition::new(x, y))
        .map_err(|e| format!("set_position: {e}"))?;
    webview
        .set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
        .map_err(|e| format!("set_size: {e}"))?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_eval(
    state: State<'_, AppState>,
    pane_id: String,
    js: String,
) -> Result<(), String> {
    let webview = state
        .browser_webviews
        .lock()
        .unwrap()
        .get(&pane_id)
        .cloned()
        .ok_or_else(|| format!("no browser webview for pane {pane_id}"))?;
    webview.eval(js).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_hide(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let webview = state.browser_webviews.lock().unwrap().get(&pane_id).cloned();
    if let Some(w) = webview {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn browser_pane_show(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let webview = state.browser_webviews.lock().unwrap().get(&pane_id).cloned();
    if let Some(w) = webview {
        w.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `workspace_delete` hooks here to wipe the per-workspace session dir
/// (the user explicitly deleted the workspace — they don't want
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
            "[browser_pane] cleaned sessions dir for ws={} at {}",
            workspace_id,
            dir.display()
        )),
        Err(e) => dlog(&format!(
            "[browser_pane] FAILED to clean sessions dir {}: {}",
            dir.display(),
            e
        )),
    }
}
