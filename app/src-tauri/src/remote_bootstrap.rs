//! Phase 51.D: thin shim around `winmux-bootstrap`.
//!
//! The actual CLI deploy logic moved to its own crate; this module
//! does only the Tauri-specific resource resolution (manifest +
//! bundled Linux binary lookup via `app.path().resolve(...)`) and
//! delegates the russh + SFTP work to `winmux_bootstrap::bootstrap`.

use tauri::{AppHandle, Manager};

use crate::dlog;
use crate::SshClient;

// Re-export the public surface so existing crate::remote_bootstrap::*
// callsites continue to resolve unchanged.
pub use winmux_bootstrap::{BootstrapStatus, ManifestEntry, PATH_RC_SNIPPET};

fn read_manifest_text(app: &AppHandle) -> Result<String, String> {
    let path = app
        .path()
        .resolve(
            "resources/remote-manifest.json",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("resolve manifest: {e}"))?;
    dlog(&format!("bootstrap: manifest path = {:?} exists={}", path, path.exists()));
    std::fs::read_to_string(&path).map_err(|e| format!("read manifest: {e}"))
}

fn read_resource_bytes(app: &AppHandle, rel: &str) -> Result<Vec<u8>, String> {
    let path = app
        .path()
        .resolve(format!("resources/{}", rel), tauri::path::BaseDirectory::Resource)
        .map_err(|e| format!("resolve {rel}: {e}"))?;
    dlog(&format!(
        "bootstrap: binary resource path = {:?} exists={}",
        path,
        path.exists()
    ));
    let bytes = std::fs::read(&path).map_err(|e| format!("read {rel}: {e}"))?;
    dlog(&format!("bootstrap: read {} bytes from {:?}", bytes.len(), path));
    Ok(bytes)
}

/// Phase 6.2 → Phase 51.D shim: resolve Tauri resources, then hand off
/// to `winmux_bootstrap::bootstrap` for the russh+sftp work.
pub async fn bootstrap(
    handle: &mut russh::client::Handle<SshClient>,
    app: &AppHandle,
    force: bool,
) -> Result<BootstrapStatus, String> {
    let manifest_text = read_manifest_text(app)?;
    dlog(&format!(
        "bootstrap: manifest read {} bytes",
        manifest_text.len()
    ));
    let manifest = winmux_bootstrap::parse_manifest(&manifest_text)?;
    let app_for_loader = app.clone();
    let loader = move |rel: &str| read_resource_bytes(&app_for_loader, rel);
    winmux_bootstrap::bootstrap(handle, manifest, &loader, force).await
}
