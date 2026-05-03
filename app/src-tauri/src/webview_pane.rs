// Phase 8.D.1: native WebView2 child-webviews backing browser panes (instead
// of <iframe>). Tauri 2 exposes `Window::add_child(WebviewBuilder, pos, size)`
// behind the `unstable` feature flag — we enable it in Cargo.toml.
//
// Coordinate model: positions and sizes are LOGICAL pixels (DPI-independent),
// relative to the OS window's content area. The frontend owns layout via
// `getBoundingClientRect()` and pushes (x, y, w, h) updates here on resize.
//
// Lifetime: the frontend creates one webview per browser pane on mount,
// repositions on resize, hides when the workspace switches away, shows when
// it returns, and destroys on close. We keep the Webview handle alive in a
// per-app HashMap so subsequent navigate / eval calls find it again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Webview, WebviewBuilder, WebviewUrl, Wry,
};

pub(crate) type WebviewPaneMap = Arc<Mutex<HashMap<String, Webview<Wry>>>>;

const MAIN_WINDOW: &str = "main";

fn label_for(pane_id: &str) -> String {
    format!("winmux-browser-{pane_id}")
}

/// Idempotent: ensures a child webview exists for the pane, sized + positioned
/// where the placeholder asked, visible, and pointed at `url`. If a webview
/// already exists, repositions + shows + navigates. Otherwise creates a fresh
/// one. The caller (BrowserPane) ONLY calls this once a successful URL
/// resolve has produced a valid target — when SSH isn't ready yet and resolve
/// fails, no webview is created at all so the user sees the waiting overlay
/// instead of a "can't connect" page.
pub(crate) fn ensure(
    app: &AppHandle,
    map: &WebviewPaneMap,
    pane_id: &str,
    url: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url {url}: {e}"))?;
    {
        let m = map.lock().unwrap();
        if let Some(v) = m.get(pane_id) {
            v.set_position(LogicalPosition::new(x, y))
                .map_err(|e| format!("set_position: {e}"))?;
            v.set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
                .map_err(|e| format!("set_size: {e}"))?;
            v.show().map_err(|e| format!("show: {e}"))?;
            // Navigate even if URL is unchanged — caller wants whatever URL
            // they passed to be the live one. Webview2 is smart enough to
            // skip a no-op navigation in most cases.
            v.navigate(parsed)
                .map_err(|e| format!("navigate: {e}"))?;
            return Ok(label_for(pane_id));
        }
    }
    let window = app
        .get_window(MAIN_WINDOW)
        .ok_or_else(|| format!("no '{MAIN_WINDOW}' window"))?;
    let label = label_for(pane_id);
    let builder: WebviewBuilder<Wry> =
        WebviewBuilder::new(label.clone(), WebviewUrl::External(parsed));
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(w.max(1.0), h.max(1.0)),
        )
        .map_err(|e| format!("add_child {label}: {e}"))?;
    map.lock().unwrap().insert(pane_id.to_string(), webview);
    Ok(label)
}

pub(crate) fn position(
    map: &WebviewPaneMap,
    pane_id: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let m = map.lock().unwrap();
    let v = m
        .get(pane_id)
        .ok_or_else(|| format!("no webview for pane {pane_id}"))?;
    v.set_position(LogicalPosition::new(x, y))
        .map_err(|e| format!("set_position: {e}"))?;
    v.set_size(LogicalSize::new(w.max(1.0), h.max(1.0)))
        .map_err(|e| format!("set_size: {e}"))?;
    Ok(())
}

pub(crate) fn navigate(map: &WebviewPaneMap, pane_id: &str, url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url {url}: {e}"))?;
    let m = map.lock().unwrap();
    let v = m
        .get(pane_id)
        .ok_or_else(|| format!("no webview for pane {pane_id}"))?;
    v.navigate(parsed).map_err(|e| format!("navigate: {e}"))
}

pub(crate) fn hide(map: &WebviewPaneMap, pane_id: &str) -> Result<(), String> {
    let m = map.lock().unwrap();
    if let Some(v) = m.get(pane_id) {
        v.hide().map_err(|e| format!("hide: {e}"))?;
    }
    Ok(())
}

pub(crate) fn show(map: &WebviewPaneMap, pane_id: &str) -> Result<(), String> {
    let m = map.lock().unwrap();
    if let Some(v) = m.get(pane_id) {
        v.show().map_err(|e| format!("show: {e}"))?;
    }
    Ok(())
}

pub(crate) fn destroy(map: &WebviewPaneMap, pane_id: &str) -> Result<(), String> {
    let v = map.lock().unwrap().remove(pane_id);
    if let Some(v) = v {
        v.close().map_err(|e| format!("close: {e}"))?;
    }
    Ok(())
}

/// Fire-and-forget JavaScript evaluation in the pane's webview. Same-origin
/// limitations of iframe.contentWindow.eval no longer apply because the
/// webview IS the origin. Phase 8.D.1 doesn't yet plumb a return value back
/// — that lands in 8.D.3 (CDP / MCP bridge); for now `winmux dev`-style
/// debug is enough.
pub(crate) fn eval(map: &WebviewPaneMap, pane_id: &str, script: &str) -> Result<(), String> {
    let m = map.lock().unwrap();
    let v = m
        .get(pane_id)
        .ok_or_else(|| format!("no webview for pane {pane_id}"))?;
    v.eval(script).map_err(|e| format!("eval: {e}"))
}
