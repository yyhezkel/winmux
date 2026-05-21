//! Phase 9.B: lightweight update checker.
//!
//! No actual download / install — that requires signing keys we don't have
//! yet. We just fetch a remote `manifest.json`, compare the version, and
//! emit `update:available` so the frontend can toast a "new version
//! available — release notes" link.
//!
//! The manifest URL lives in `settings.updates.manifest_url` so it can be
//! switched without recompiling. Until the repo goes public the default
//! placeholder URL will return 404 / DNS-fail; the failure is silent and
//! never blocks startup.
//!
//! Fetch path: shell out to PowerShell `Invoke-WebRequest` so we get TLS
//! / proxy / CRL handling for free without pulling reqwest + tokio-rustls
//! into the dep tree (Tauri's binary size is already 60 MB+).

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use crate::{dlog, AppState};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: String,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub notes_url: Option<String>,
    #[serde(default)]
    pub msi_url: Option<String>,
    #[serde(default)]
    pub msi_sha256: Option<String>,
    // Phase 27: NSIS installer URL + sha256, used by
    // `download_and_install_update` for one-click auto-install.
    // NSIS is preferred over MSI because it handles "app is running"
    // gracefully (wait / retry / replace) without requiring elevation
    // every time. Older manifests without these fields fall back to
    // the manual download path (Release notes link).
    #[serde(default)]
    pub nsis_url: Option<String>,
    #[serde(default)]
    pub nsis_sha256: Option<String>,
    #[serde(default)]
    pub min_supported_version: Option<String>,
    /// Phase 18: per-agent hook spec versions. Map keyed by agent
    /// id (`"claude-code"`, `"codex"`, `"gemini"`). Pre-18 manifests
    /// without this field load fine — the desktop's hooks-outdated
    /// check just no-ops.
    #[serde(default)]
    pub hooks: std::collections::BTreeMap<String, ManifestHook>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ManifestHook {
    pub version: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub min_winmux_version: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct UpdateInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msi_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub last_check_iso: String,
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Compare two semver-ish version strings (`major.minor.patch[-suffix]`).
/// Returns Greater iff `a` is strictly newer than `b`. Suffixes are
/// compared lexically as a tiebreaker (`""` < `"alpha"` < `"beta"`...).
fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let split = |v: &str| -> (Vec<u64>, String) {
        let (head, tail) = match v.split_once('-') {
            Some((h, t)) => (h, t.to_string()),
            None => (v, String::new()),
        };
        let nums: Vec<u64> = head
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (nums, tail)
    };
    let (an, asfx) = split(a);
    let (bn, bsfx) = split(b);
    for i in 0..an.len().max(bn.len()) {
        let x = *an.get(i).unwrap_or(&0);
        let y = *bn.get(i).unwrap_or(&0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    match (asfx.is_empty(), bsfx.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => asfx.cmp(&bsfx),
    }
}

async fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    let req_url = url.to_string();
    let body = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                // PowerShell handles HTTPS + redirects + system proxy / CA store
                // for us. -UseBasicParsing avoids the IE engine dependency on
                // Windows Server / fresh Win11 installs.
                "$ProgressPreference = 'SilentlyContinue'; \
                 try { (Invoke-WebRequest -Uri $env:WINMUX_MANIFEST_URL -UseBasicParsing -TimeoutSec 8).Content } \
                 catch { Write-Error $_.Exception.Message; exit 1 }",
            ])
            .env("WINMUX_MANIFEST_URL", &req_url)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("spawn powershell: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            return Err(err.lines().next().unwrap_or("powershell error").to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    })
    .await
    .map_err(|e| format!("join: {e}"))??;

    let m: Manifest = serde_json::from_str(body.trim_start_matches('\u{FEFF}').trim())
        .map_err(|e| format!("parse manifest: {e}"))?;
    Ok(m)
}

/// Check the manifest URL from settings, emit `update:available` if a
/// newer version is found, persist last_check_iso + last_seen_version.
/// Used both by the startup task and the manual `check_for_updates_now`
/// Tauri command.
pub(crate) async fn check(state: &AppState, app: &AppHandle) -> UpdateInfo {
    let (url, last_seen) = {
        let s = state.settings.lock().unwrap();
        (s.updates.manifest_url.clone(), s.updates.last_seen_version.clone())
    };
    let now = iso_now();

    let url = match url {
        Some(u) if !u.is_empty() => u,
        _ => {
            return UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: None,
                available: false,
                notes_url: None,
                msi_url: None,
                released_at: None,
                manifest_url: None,
                error: Some("manifest_url not configured".into()),
                last_check_iso: now,
            };
        }
    };

    let info = match fetch_manifest(&url).await {
        Ok(m) => {
            let available = matches!(
                cmp_versions(&m.version, APP_VERSION),
                std::cmp::Ordering::Greater
            );
            UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: Some(m.version.clone()),
                available,
                notes_url: m.notes_url.clone(),
                msi_url: m.msi_url.clone(),
                released_at: m.released_at.clone(),
                manifest_url: Some(url.clone()),
                error: None,
                last_check_iso: now.clone(),
            }
        }
        Err(e) => {
            dlog(&format!("updater: fetch {url} failed: {e}"));
            UpdateInfo {
                current_version: APP_VERSION.into(),
                latest_version: None,
                available: false,
                notes_url: None,
                msi_url: None,
                released_at: None,
                manifest_url: Some(url.clone()),
                error: Some(e),
                last_check_iso: now.clone(),
            }
        }
    };

    {
        let mut s = state.settings.lock().unwrap();
        s.updates.last_check_iso = Some(now.clone());
        if let Some(v) = info.latest_version.clone() {
            s.updates.last_seen_version = Some(v);
        }
        let _ = crate::settings::save_to_disk_pub(&s);
    }

    if info.available && info.latest_version != last_seen {
        dlog(&format!(
            "updater: new version {} available (current {})",
            info.latest_version.clone().unwrap_or_default(),
            APP_VERSION
        ));
        let _ = app.emit("update:available", &info);
    }
    info
}

#[tauri::command]
pub(crate) async fn check_for_updates_now(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<UpdateInfo, String> {
    Ok(check(&state, &app).await)
}

// ─── Phase 18: hooks-outdated probe ────────────────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct HooksOutdatedInfo {
    pub workspace_id: String,
    pub pane_id: String,
    pub agent: String,
    pub current: Option<String>,
    pub latest: String,
}

/// Read the cached manifest (fetching if absent), then over the
/// established SSH handle ask the remote what version it has
/// installed. Emit `hooks:outdated` when the remote's version is
/// older AND the user hasn't dismissed it.
pub(crate) async fn check_remote_hooks(
    state: &AppState,
    app: &AppHandle,
    handle: &std::sync::Arc<russh::client::Handle<crate::SshClient>>,
    workspace_id: &str,
    pane_id: &str,
) {
    // 1. Fetch the latest manifest (cached in last_check…). Cheap if
    //    cached, otherwise a single curl call.
    let manifest_url = match state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.updates.manifest_url.clone())
    {
        Some(u) if !u.is_empty() => u,
        _ => return,
    };
    let manifest = match fetch_manifest(&manifest_url).await {
        Ok(m) => m,
        Err(e) => {
            dlog(&format!("hooks-check: fetch manifest failed: {e}"));
            return;
        }
    };
    let claude_latest = match manifest.hooks.get("claude-code") {
        Some(h) => h.version.clone(),
        None => return,
    };

    // 2. Ask the remote what `winmux_meta.hooks_version` is in
    //    ~/.claude/settings.json. `jq` does the lookup cleanly; we
    //    fall back to grep for hosts without jq.
    let cmd = "if [ -f \"$HOME/.claude/settings.json\" ]; then \
               if command -v jq >/dev/null; then \
                 jq -r '.winmux_meta.hooks_version // \"none\"' \"$HOME/.claude/settings.json\" 2>/dev/null; \
               else \
                 grep -oE '\"hooks_version\"\\s*:\\s*\"[^\"]+\"' \"$HOME/.claude/settings.json\" 2>/dev/null \
                   | head -1 | sed -E 's/.*\"([^\"]+)\"$/\\1/'; \
               fi; \
             else echo MISSING; fi";
    let current = match ssh_exec_simple(handle, cmd).await {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            dlog(&format!("hooks-check: remote read failed: {e}"));
            return;
        }
    };
    let current_opt: Option<String> = match current.as_str() {
        "" | "MISSING" | "none" | "null" => None,
        other => Some(other.to_string()),
    };

    // 3. Compare. None → hooks never installed (caller may want
    //    "install now" prompt later; for outdated banner we skip).
    //    Older → emit. Same/newer → no-op.
    let need_banner = match &current_opt {
        Some(cur) => matches!(cmp_versions(&claude_latest, cur), std::cmp::Ordering::Greater),
        None => false,
    };
    if !need_banner {
        dlog(&format!(
            "hooks-check: workspace={workspace_id} agent=claude-code current={current_opt:?} latest={claude_latest} → up-to-date or missing"
        ));
        return;
    }

    // 4. Honor the user's dismissed list.
    let dismissed = state
        .settings
        .lock()
        .ok()
        .and_then(|s| {
            s.hooks_updates
                .dismissed
                .get("claude-code")
                .cloned()
        })
        .unwrap_or_default();
    if dismissed.contains(&claude_latest) {
        dlog(&format!(
            "hooks-check: workspace={workspace_id} claude-code v{claude_latest} silently dismissed"
        ));
        return;
    }
    let show_banners = state
        .settings
        .lock()
        .ok()
        .map(|s| s.hooks_updates.show_banners)
        .unwrap_or(true);
    if !show_banners {
        return;
    }

    let info = HooksOutdatedInfo {
        workspace_id: workspace_id.to_string(),
        pane_id: pane_id.to_string(),
        agent: "claude-code".to_string(),
        current: current_opt,
        latest: claude_latest,
    };
    let _ = app.emit("hooks:outdated", &info);
}

/// Phase 18: run an arbitrary shell command in a workspace's active
/// SSH session. Used by the hooks-banner "Update now" button to fire
/// `winmux setup-hooks --agent claude --force --source github` over
/// the tunnel without making the user open a pane and type it.
/// Errors when no SSH session is alive for the workspace.
#[tauri::command]
pub(crate) async fn ssh_exec_in_workspace(
    state: tauri::State<'_, AppState>,
    workspace_id: String,
    cmd: String,
) -> Result<String, String> {
    let handle = {
        let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        sessions.values().find_map(|s| match s {
            crate::Session::Ssh(ssh) if ssh.workspace_id == workspace_id => {
                Some(ssh.handle.clone())
            }
            _ => None,
        })
    }
    .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
    ssh_exec_simple(&handle, &cmd).await
}

async fn ssh_exec_simple(
    handle: &std::sync::Arc<russh::client::Handle<crate::SshClient>>,
    cmd: &str,
) -> Result<String, String> {
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(6), async {
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

/// Helper for CLI / RPC dispatch — not a Tauri command.
pub(crate) async fn rpc_check_now(state: &AppState, app: &AppHandle) -> serde_json::Value {
    let info = check(state, app).await;
    json!({
        "current_version": info.current_version,
        "latest_version": info.latest_version,
        "available": info.available,
        "notes_url": info.notes_url,
        "msi_url": info.msi_url,
        "released_at": info.released_at,
        "last_check_iso": info.last_check_iso,
        "error": info.error,
    })
}

// ─── Phase 27: one-click auto-install ──────────────────────────────────────

/// Download a URL to a local path using PowerShell's Invoke-WebRequest.
/// Mirrors the manifest-fetch path (TLS / proxy / CA store handled by
/// PowerShell, no reqwest dep) but writes to disk instead of capturing
/// to stdout — necessary for multi-MB installer downloads.
async fn powershell_download(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let req_url = url.to_string();
    let dest_path = dest.to_string_lossy().to_string();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ProgressPreference = 'SilentlyContinue'; \
                 try { Invoke-WebRequest -Uri $env:WINMUX_DL_URL -OutFile $env:WINMUX_DL_DEST -UseBasicParsing -TimeoutSec 120 } \
                 catch { Write-Error $_.Exception.Message; exit 1 }",
            ])
            .env("WINMUX_DL_URL", &req_url)
            .env("WINMUX_DL_DEST", &dest_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("spawn powershell: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            return Err(err.lines().next().unwrap_or("powershell error").to_string());
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))??;
    Ok(())
}

/// sha256 of a file, lowercase hex. Reads the whole file into memory
/// — fine for the ~5-7 MB NSIS installer; we'd stream for anything
/// bigger.
fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

/// Phase 27: re-fetch the manifest, download the NSIS installer
/// listed in `nsis_url`, verify its sha256 against
/// `manifest.nsis_sha256`, spawn the installer detached, then exit
/// the app ~800ms later so the spawned process settles before the
/// parent goes away.
///
/// Integrity: a wrong sha256 deletes the temp file and aborts. This
/// gives us download integrity but NOT publisher authenticity — the
/// installer is unsigned (SmartScreen will warn on first launch).
/// Code-signing is a separate future task; see RELEASING.md
/// "Caveats" section.
#[tauri::command]
pub(crate) async fn download_and_install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Step 1: re-fetch the manifest (we don't trust the cached one
    // from the last check — the version might have moved on, and
    // we'd rather error than install something stale).
    let url = {
        let s = state.settings.lock().unwrap();
        s.updates
            .manifest_url
            .clone()
            .ok_or_else(|| "no manifest_url configured".to_string())?
    };
    let manifest = fetch_manifest(&url).await?;

    // Step 2: guards.
    let nsis_url = manifest
        .nsis_url
        .clone()
        .ok_or_else(|| "manifest has no nsis_url — falling back to manual download".to_string())?;
    let expected_sha = manifest
        .nsis_sha256
        .clone()
        .ok_or_else(|| "manifest has no nsis_sha256 — refusing to install unverified".to_string())?;
    if cmp_versions(&manifest.version, APP_VERSION) != std::cmp::Ordering::Greater {
        return Err(format!(
            "manifest version {} is not newer than current {APP_VERSION} — nothing to install",
            manifest.version
        ));
    }

    // Step 3: download to %TEMP%.
    let temp_dir = std::env::temp_dir();
    let dest = temp_dir.join(format!("winmux-update-{}.exe", manifest.version));
    dlog(&format!(
        "updater: downloading {} -> {:?} (expected sha256 {expected_sha})",
        nsis_url, dest
    ));
    powershell_download(&nsis_url, &dest)
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    // Step 4: integrity check.
    let actual_sha = sha256_file(&dest)
        .map_err(|e| format!("hash failed: {e}"))?;
    if !actual_sha.eq_ignore_ascii_case(&expected_sha) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "downloaded installer failed integrity check — expected {expected_sha}, got {actual_sha} — aborting"
        ));
    }
    dlog(&format!("updater: sha256 verified ({actual_sha})"));

    // Step 5: spawn the installer detached and schedule app exit.
    // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS so the installer
    // survives our exit and isn't tied to our console.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        std::process::Command::new(&dest)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("spawn installer: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new(&dest)
            .spawn()
            .map_err(|e| format!("spawn installer: {e}"))?;
    }
    dlog("updater: installer spawned — scheduling app exit in 800ms");

    // Step 6: schedule exit. We can't exit synchronously here because
    // the FE needs the Ok(()) return to fire before we go away
    // (otherwise the invoke promise rejects with a window-destroyed
    // error and the user sees a misleading toast).
    let app_clone = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        app_clone.exit(0);
    });
    Ok(())
}
