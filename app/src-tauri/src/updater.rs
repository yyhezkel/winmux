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
    #[serde(default)]
    pub min_supported_version: Option<String>,
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
