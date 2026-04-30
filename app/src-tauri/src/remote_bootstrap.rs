//! Phase 6.2: bootstrap the winmux Linux binary on a remote SSH server.
//!
//! Best-effort. Called after auth succeeds, before opening the user's shell channel.
//! Detects the remote arch, hashes the existing binary (if any), and uploads via SFTP
//! when the hash doesn't match the manifest. Maintains a `~/.winmux/bin/winmux`
//! symlink to the architecture-specific binary.

use std::collections::HashMap;

use russh::client::Handle;
use russh::ChannelMsg;
use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::dlog;
use crate::SshClient;

const REMOTE_DIR: &str = ".winmux/bin";

#[derive(Deserialize, Debug)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
    #[allow(dead_code)]
    pub size: u64,
    #[allow(dead_code)]
    pub built_at: String,
}

#[derive(Debug)]
pub enum BootstrapStatus {
    AlreadyOk,
    Uploaded {
        bytes: usize,
        #[allow(dead_code)]
        sha256: String,
    },
    UnsupportedArch(String),
}

fn read_manifest(app: &AppHandle) -> Result<HashMap<String, ManifestEntry>, String> {
    let path = app
        .path()
        .resolve(
            "resources/remote-manifest.json",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("resolve manifest: {e}"))?;
    dlog(&format!("bootstrap: manifest path = {:?} exists={}", path, path.exists()));
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read manifest: {e}"))?;
    // Defensive: strip a UTF-8 BOM (\u{FEFF}) if the writer (e.g. PowerShell 5.1's
    // `Set-Content -Encoding utf8`) tacked one on. serde_json otherwise fails with
    // "expected value at line 1 column 1".
    let text = raw.trim_start_matches('\u{FEFF}');
    dlog(&format!(
        "bootstrap: manifest read {} bytes (after BOM strip: {} bytes)",
        raw.len(),
        text.len()
    ));
    serde_json::from_str(text).map_err(|e| format!("parse manifest: {e}"))
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

async fn ssh_exec(
    handle: &mut Handle<SshClient>,
    cmd: &str,
) -> Result<(String, i32), String> {
    dlog(&format!("bootstrap: exec '{}'", cmd));
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open exec channel: {e}"))?;
    chan.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                stderr.extend_from_slice(&data[..])
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    let stdout_str = String::from_utf8_lossy(&stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr).to_string();
    dlog(&format!(
        "bootstrap: exec '{}' exit={} stdout={:?} stderr={:?}",
        cmd,
        exit_code,
        stdout_str.trim(),
        stderr_str.trim()
    ));
    Ok((stdout_str, exit_code))
}

fn detect_triple(uname_output: &str) -> Option<&'static str> {
    let s = uname_output.trim();
    match s {
        "Linux x86_64" => Some("x86_64-linux"),
        "Linux aarch64" => Some("aarch64-linux"),
        _ => None,
    }
}

async fn upload_via_sftp(
    handle: &mut Handle<SshClient>,
    abs_remote_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    dlog(&format!(
        "bootstrap: opening sftp subsystem for {} ({} bytes)",
        abs_remote_path,
        bytes.len()
    ));
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: sftp channel_open failed: {e}"));
            format!("open sftp channel: {e}")
        })?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: sftp request_subsystem failed: {e}"));
            format!("request sftp: {e}")
        })?;
    let stream = chan.into_stream();
    let sftp = russh_sftp::client::SftpSession::new(stream)
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: SftpSession::new failed: {e}"));
            format!("sftp init: {e}")
        })?;
    dlog("bootstrap: sftp session ready");

    {
        let mut file = sftp
            .create(abs_remote_path)
            .await
            .map_err(|e| {
                dlog(&format!("bootstrap: sftp.create {abs_remote_path} failed: {e}"));
                format!("sftp create {abs_remote_path}: {e}")
            })?;
        file.write_all(bytes)
            .await
            .map_err(|e| {
                dlog(&format!("bootstrap: sftp write_all failed: {e}"));
                format!("sftp write: {e}")
            })?;
        file.flush().await.ok();
        file.shutdown().await.ok();
    }
    dlog("bootstrap: sftp upload complete");

    let _ = sftp.close().await;
    Ok(())
}

pub async fn bootstrap(
    handle: &mut Handle<SshClient>,
    app: &AppHandle,
    force: bool,
) -> Result<BootstrapStatus, String> {
    dlog(&format!("bootstrap: starting (force={force})"));

    // Identify remote.
    let (uname, code) = ssh_exec(handle, "uname -s -m").await?;
    if code != 0 {
        return Err(format!("uname failed: exit {code}"));
    }
    let triple = match detect_triple(&uname) {
        Some(t) => t,
        None => {
            dlog(&format!("bootstrap: unsupported arch '{}'", uname.trim()));
            return Ok(BootstrapStatus::UnsupportedArch(uname.trim().to_string()));
        }
    };
    dlog(&format!("bootstrap: triple = {}", triple));

    // Resolve manifest entry for this triple.
    let manifest = read_manifest(app)?;
    let entry = manifest
        .get(triple)
        .ok_or_else(|| format!("no manifest entry for {triple}"))?;
    dlog(&format!(
        "bootstrap: manifest entry path={} sha256={}",
        entry.path, entry.sha256
    ));

    // Get remote $HOME so SFTP gets an absolute path.
    let (home_out, _) = ssh_exec(handle, "echo $HOME").await?;
    let home = home_out.trim();
    if home.is_empty() {
        return Err("empty $HOME on remote".into());
    }
    let remote_dir_abs = format!("{}/{}", home, REMOTE_DIR);
    let remote_bin_abs = format!("{}/{}", remote_dir_abs, entry.path);
    let remote_symlink_abs = format!("{}/winmux", remote_dir_abs);
    dlog(&format!(
        "bootstrap: remote paths — dir={} bin={} symlink={}",
        remote_dir_abs, remote_bin_abs, remote_symlink_abs
    ));

    // Compare existing hash unless forced.
    if !force {
        let (sum_out, _) = ssh_exec(
            handle,
            &format!("sha256sum {remote_bin_abs} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await?;
        let remote_hash = sum_out.trim().to_lowercase();
        if remote_hash == entry.sha256.to_lowercase() {
            dlog("bootstrap: hash matches existing — skipping upload");
            // Ensure symlink anyway.
            let _ = ssh_exec(
                handle,
                &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
            )
            .await;
            return Ok(BootstrapStatus::AlreadyOk);
        }
        dlog(&format!(
            "bootstrap: hash mismatch — remote='{}' expected='{}' — will upload",
            remote_hash, entry.sha256
        ));
    }

    // Make dir, upload, chmod, symlink.
    ssh_exec(handle, &format!("mkdir -p {remote_dir_abs}")).await?;

    let bytes = read_resource_bytes(app, &entry.path)?;
    upload_via_sftp(handle, &remote_bin_abs, &bytes).await?;
    ssh_exec(handle, &format!("chmod 0755 {remote_bin_abs}")).await?;
    ssh_exec(
        handle,
        &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
    )
    .await?;

    // Verify post-upload.
    let (verify_out, _) = ssh_exec(
        handle,
        &format!("sha256sum {remote_bin_abs} | awk '{{print $1}}'"),
    )
    .await?;
    let after_hash = verify_out.trim().to_lowercase();
    if after_hash != entry.sha256.to_lowercase() {
        dlog(&format!(
            "bootstrap: FAILED post-upload hash mismatch: got {} expected {}",
            after_hash, entry.sha256
        ));
        return Err(format!(
            "post-upload hash mismatch: got {after_hash}, expected {}",
            entry.sha256
        ));
    }
    dlog("bootstrap: COMPLETE — upload verified");

    Ok(BootstrapStatus::Uploaded {
        bytes: bytes.len(),
        sha256: entry.sha256.clone(),
    })
}
