//! Phase 15.B: file manager pane (local + remote SFTP).
//!
//! Backend for the dual-column file manager: lists, transfers, and
//! mutations on both sides of the divider. Local ops use std::fs;
//! remote ops piggy-back on an already-connected SSH session for the
//! workspace, opening a fresh SFTP channel each call. Sessions are not
//! cached — a fresh SFTP subsystem per op is cheap on an existing
//! authenticated handle and avoids us having to chase teardown
//! semantics when the terminal pane disconnects.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use russh::client::Handle as SshHandle;
use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{AppState, Session, SshClient};

/// Phase 75.3: progress tick for an in-flight download, emitted to the
/// frontend so a large transfer shows movement instead of a frozen pane.
#[derive(Clone, Serialize)]
struct DownloadProgress {
    path: String,
    done: u64,
    total: u64, // 0 if the remote size couldn't be stat'd
}

/// Stream a remote file to `local` in chunks, emitting throttled
/// `fm-download-progress` events. Returns bytes written. Avoids buffering the
/// whole file in RAM (the old read_to_end) — important for multi-GB files —
/// and gives the UI a live byte/percent readout.
async fn stream_download(
    app: &AppHandle,
    sftp: &SftpSession,
    remote_path: &str,
    local: &std::path::Path,
) -> Result<u64, String> {
    let total = sftp
        .metadata(remote_path)
        .await
        .ok()
        .and_then(|m| m.size)
        .unwrap_or(0);
    crate::dlog_tag("FM", &format!("download begin remote={remote_path} size={total}"));
    let mut file = sftp
        .open(remote_path)
        .await
        .map_err(|e| format!("sftp open {remote_path}: {e}"))?;
    let mut out = tokio::fs::File::create(local)
        .await
        .map_err(|e| format!("create {local:?}: {e}"))?;
    let emit = |done: u64, total: u64| {
        let _ = app.emit(
            "fm-download-progress",
            DownloadProgress { path: remote_path.to_string(), done, total },
        );
    };
    let mut done: u64 = 0;
    let mut chunk = vec![0u8; 256 * 1024];
    let mut last = std::time::Instant::now();
    let mut last_done = 0u64;
    loop {
        let n = file.read(&mut chunk).await.map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&chunk[..n]).await.map_err(|e| format!("write: {e}"))?;
        done += n as u64;
        // Throttle emits: ~every 300 ms or every 4 MB, whichever first.
        if last.elapsed().as_millis() >= 300 || done - last_done >= 4 * 1024 * 1024 {
            emit(done, total);
            last = std::time::Instant::now();
            last_done = done;
        }
    }
    out.flush().await.ok();
    emit(done, total.max(done)); // final 100% tick
    Ok(done)
}

/// Phase 75.2: log a file-manager op's outcome to debug.log under `[FM]` so a
/// failed transfer/mutation is diagnosable instead of vanishing behind a
/// corner toast. Wrap the op's Result: `fm_log("download", &detail, res)`.
/// Only paths + sizes are logged (metadata — Rule #1 safe); never file bytes.
fn fm_log<T>(op: &str, detail: &str, res: Result<T, String>) -> Result<T, String> {
    match &res {
        Ok(_) => crate::dlog_tag("FM", &format!("{op} ok — {detail}")),
        Err(e) => crate::dlog_tag("FM", &format!("{op} FAILED — {detail}: {e}")),
    }
    res
}

// ─── data shapes ───────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    /// Bytes. 0 for directories.
    pub size: u64,
    /// Unix timestamp, seconds. 0 if unknown.
    pub modified: i64,
    /// Unix octal ("0644") for remote; Windows attributes summary
    /// ("rwx" / "r-x") for local. Best-effort, always present.
    pub permissions: String,
}

// ─── local-side ────────────────────────────────────────────────────────────

fn iso_unix(st: SystemTime) -> i64 {
    st.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn local_perms(md: &std::fs::Metadata) -> String {
    let ro = md.permissions().readonly();
    if md.is_dir() {
        if ro { "r-x".into() } else { "rwx".into() }
    } else if ro {
        "r--".into()
    } else {
        "rw-".into()
    }
}

#[tauri::command]
pub(crate) fn file_list_local(path: String, show_hidden: bool) -> Result<Vec<FileEntry>, String> {
    let p = expand_path(&path);
    let p = PathBuf::from(p);
    let mut out: Vec<FileEntry> = Vec::new();
    let read_dir = std::fs::read_dir(&p).map_err(|e| format!("read_dir {p:?}: {e}"))?;
    for ent in read_dir.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        // Symlink-aware metadata first; if it's a link, fall back to
        // metadata() for type info but keep is_link = true.
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let symlink_md = ent.path().symlink_metadata().ok();
        let is_link = symlink_md.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
        let size = if md.is_dir() { 0 } else { md.len() };
        let modified = md
            .modified()
            .ok()
            .map(iso_unix)
            .unwrap_or(0);
        out.push(FileEntry {
            name,
            is_dir: md.is_dir(),
            is_link,
            size,
            modified,
            permissions: local_perms(&md),
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

#[tauri::command]
pub(crate) fn file_delete_local(path: String) -> Result<(), String> {
    let p = PathBuf::from(expand_path(&path));
    if p.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| format!("remove_dir_all {p:?}: {e}"))
    } else {
        std::fs::remove_file(&p).map_err(|e| format!("remove_file {p:?}: {e}"))
    }
}

#[tauri::command]
pub(crate) fn file_rename_local(old_path: String, new_path: String) -> Result<(), String> {
    let o = PathBuf::from(expand_path(&old_path));
    let n = PathBuf::from(expand_path(&new_path));
    std::fs::rename(&o, &n).map_err(|e| format!("rename {o:?} -> {n:?}: {e}"))
}

#[tauri::command]
pub(crate) fn file_mkdir_local(path: String) -> Result<(), String> {
    let p = PathBuf::from(expand_path(&path));
    std::fs::create_dir_all(&p).map_err(|e| format!("mkdir {p:?}: {e}"))
}

/// Unshipped-fivefer (#5): copy a local file into the file manager's local
/// column. `std::fs::copy` handles binary content and any size natively (no
/// read-as-text + write round-trip through IPC), removing the old "binary
/// drop not supported" limitation. Folders aren't supported yet.
#[tauri::command]
pub(crate) fn file_copy_local(src: String, dest: String) -> Result<(), String> {
    let s = PathBuf::from(expand_path(&src));
    let d = PathBuf::from(expand_path(&dest));
    if s.is_dir() {
        return Err(format!("copying folders isn't supported yet: {}", s.display()));
    }
    if let Some(parent) = d.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
        }
    }
    std::fs::copy(&s, &d)
        .map(|_| ())
        .map_err(|e| format!("copy {s:?} -> {d:?}: {e}"))
}

/// Phase 23: create a new empty file locally. Fails if the path already
/// exists (we never silently truncate an existing file from a "New
/// file" UI gesture — that's a recipe for data loss).
#[tauri::command]
pub(crate) fn file_create_local(path: String) -> Result<(), String> {
    let p = PathBuf::from(expand_path(&path));
    if p.exists() {
        return Err(format!("already exists: {}", p.display()));
    }
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .map_err(|e| format!("create {p:?}: {e}"))?;
    Ok(())
}

#[tauri::command]
pub(crate) fn file_home_local() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\".to_string())
}

fn expand_path(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| "C:\\".to_string());
    }
    if let Some(rest) = s.strip_prefix('~') {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let r = rest.trim_start_matches(['/', '\\']);
        return format!("{home}\\{r}");
    }
    s.to_string()
}

fn sort_entries(out: &mut Vec<FileEntry>) {
    out.sort_by(|a, b| {
        // dirs first, then case-insensitive name.
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

// ─── remote SFTP-side ──────────────────────────────────────────────────────

/// Find an active SSH handle for the workspace by scanning the session
/// map for the first Session::Ssh whose workspace_id matches.
fn pick_ssh_handle_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Arc<SshHandle<SshClient>>> {
    let sessions = state.core.sessions.lock().ok()?;
    for sess in sessions.values() {
        if let Session::Ssh(s) = sess {
            if s.workspace_id == workspace_id {
                return Some(s.handle.clone());
            }
        }
    }
    None
}

async fn open_sftp(handle: &SshHandle<SshClient>) -> Result<SftpSession, String> {
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("request sftp: {e}"))?;
    let stream = chan.into_stream();
    SftpSession::new(stream)
        .await
        .map_err(|e| format!("sftp init: {e}"))
}

fn unix_perms_octal(perms: u32) -> String {
    format!("{:04o}", perms & 0o7777)
}

fn normalize_remote_path(p: &str) -> String {
    let t = p.trim();
    if t.is_empty() {
        return ".".to_string();
    }
    t.to_string()
}

#[tauri::command]
pub(crate) async fn file_list_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
    show_hidden: bool,
) -> Result<Vec<FileEntry>, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session for this workspace — connect a terminal pane first".to_string())?;
    let sftp = open_sftp(&handle).await?;
    let path = normalize_remote_path(&path);
    let mut out: Vec<FileEntry> = Vec::new();
    let entries = sftp
        .read_dir(&path)
        .await
        .map_err(|e| format!("read_dir {path:?}: {e}"))?;
    for ent in entries {
        let name = ent.file_name().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let attrs = ent.metadata();
        let is_dir = attrs.is_dir();
        let is_link = attrs.is_symlink();
        let size = attrs.size.unwrap_or(0);
        let modified = attrs.mtime.map(|m| m as i64).unwrap_or(0);
        let permissions = attrs
            .permissions
            .map(unix_perms_octal)
            .unwrap_or_else(|| "----".into());
        out.push(FileEntry {
            name,
            is_dir,
            is_link,
            size,
            modified,
            permissions,
        });
    }
    let _ = sftp.close().await;
    sort_entries(&mut out);
    Ok(out)
}

#[tauri::command]
pub(crate) async fn file_home_remote(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<String, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
    // `echo $HOME` over an exec channel — cheaper than SFTP for a one-off.
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.exec(true, "printf '%s' \"$HOME\"")
        .await
        .map_err(|e| format!("exec: {e}"))?;
    use russh::ChannelMsg;
    let mut out = Vec::new();
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

#[tauri::command]
pub(crate) async fn file_delete_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
) -> Result<(), String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let sftp = open_sftp(&handle).await?;
    // Try as file first; if that fails because it's a directory, recurse.
    let attempt_file = sftp.remove_file(&path).await;
    let result = match attempt_file {
        Ok(()) => Ok(()),
        Err(_) => recurse_rm(&sftp, &path).await,
    };
    let _ = sftp.close().await;
    fm_log("delete", &format!("path={path}"), result)
}

async fn recurse_rm(sftp: &SftpSession, path: &str) -> Result<(), String> {
    // Read entries, recurse into dirs, then rmdir self.
    let mut stack: Vec<(String, bool)> = vec![(path.to_string(), false)];
    while let Some((p, visited)) = stack.pop() {
        if visited {
            sftp.remove_dir(&p)
                .await
                .map_err(|e| format!("rmdir {p}: {e}"))?;
            continue;
        }
        let entries = sftp
            .read_dir(&p)
            .await
            .map_err(|e| format!("read_dir {p}: {e}"))?;
        stack.push((p.clone(), true));
        for ent in entries {
            let name = ent.file_name().to_string();
            if name == "." || name == ".." {
                continue;
            }
            let child = format!("{}/{}", p.trim_end_matches('/'), name);
            if ent.metadata().is_dir() {
                stack.push((child, false));
            } else {
                sftp.remove_file(&child)
                    .await
                    .map_err(|e| format!("rm {child}: {e}"))?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn file_rename_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let sftp = open_sftp(&handle).await?;
    let r = sftp
        .rename(&old_path, &new_path)
        .await
        .map_err(|e| format!("rename {old_path} -> {new_path}: {e}"));
    let _ = sftp.close().await;
    fm_log("rename", &format!("{old_path} → {new_path}"), r)
}

#[tauri::command]
pub(crate) async fn file_mkdir_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
) -> Result<(), String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let sftp = open_sftp(&handle).await?;
    let r = sftp
        .create_dir(&path)
        .await
        .map_err(|e| format!("mkdir {path}: {e}"));
    let _ = sftp.close().await;
    fm_log("mkdir", &format!("path={path}"), r)
}

/// Phase 23: create a new empty file on the remote via SFTP. Refuses
/// to overwrite an existing path — first `stat`s and bails if found,
/// so a "New file" gesture can't clobber data. Note: SFTP's `create`
/// truncates by default; the explicit pre-check is what gives us the
/// "fail if exists" semantics.
#[tauri::command]
pub(crate) async fn file_create_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
) -> Result<(), String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let sftp = open_sftp(&handle).await?;
    if sftp.metadata(&path).await.is_ok() {
        let _ = sftp.close().await;
        return Err(format!("already exists: {path}"));
    }
    let create_r = sftp
        .create(&path)
        .await
        .map_err(|e| format!("sftp create {path}: {e}"));
    let r = match create_r {
        Ok(mut file) => {
            // Touch zero bytes so the file exists on disk with size 0.
            let _ = file.flush().await;
            let _ = file.shutdown().await;
            Ok(())
        }
        Err(e) => Err(e),
    };
    let _ = sftp.close().await;
    r
}

/// Phase 23: upload arbitrary bytes (sourced from the frontend, e.g.
/// from an `<input type="file">` blob) to a remote path. Used by the
/// "Upload from disk" picker so the user can grab files outside the
/// current local-column directory without having to navigate there
/// first. The frontend sends bytes as a JSON array of u8 — that's the
/// shape Tauri's IPC bridge serializes Uint8Array into.
#[tauri::command]
pub(crate) async fn file_upload_bytes(
    state: State<'_, AppState>,
    workspace_id: String,
    remote_path: String,
    bytes: Vec<u8>,
) -> Result<u64, String> {
    let detail = format!("remote={remote_path} bytes={}", bytes.len());
    fm_log(
        "upload",
        &detail,
        async {
            let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
                .ok_or_else(|| "no active SSH session".to_string())?;
            let sftp = open_sftp(&handle).await?;
            let mut file = sftp
                .create(&remote_path)
                .await
                .map_err(|e| format!("sftp create {remote_path}: {e}"))?;
            file.write_all(&bytes)
                .await
                .map_err(|e| format!("write: {e}"))?;
            file.flush().await.ok();
            file.shutdown().await.ok();
            let n = bytes.len() as u64;
            drop(file);
            let _ = sftp.close().await;
            Ok(n)
        }
        .await,
    )
}

/// Phase 49-A: drag-drop into a Terminal pane. For SSH workspaces the
/// dropped file is uploaded via SFTP to `~/winmux-drops/<file_name>`
/// (created on demand) and the remote path is returned. The frontend
/// then types that path into the pane (single-quoted) so the user can
/// reference the just-uploaded file from their shell.
///
/// `pane_id` is accepted for log clarity; it's not used to route the
/// upload (the SSH session is per-workspace).
#[tauri::command]
pub(crate) async fn pane_upload_dropped(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
    local_path: String,
    file_name: String,
) -> Result<String, String> {
    // Sanitize file_name to its basename. The frontend already passes
    // the basename but defending against path separators here keeps the
    // command safe to call from anywhere.
    let safe = file_name
        .rsplit(|c: char| c == '/' || c == '\\')
        .next()
        .unwrap_or("");
    if safe.is_empty() || safe.starts_with('.') && safe.len() <= 2 {
        return Err("invalid file name".to_string());
    }
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let local = PathBuf::from(expand_path(&local_path));
    let bytes = std::fs::read(&local).map_err(|e| format!("read {local:?}: {e}"))?;
    let sftp = open_sftp(&handle).await?;
    // SFTP starts in the user's home dir; relative path puts us inside it.
    // create_dir errors if already present — ignore that case.
    let _ = sftp.create_dir("winmux-drops").await;
    let remote_path = format!("winmux-drops/{safe}");
    let mut file = sftp
        .create(&remote_path)
        .await
        .map_err(|e| format!("sftp create {remote_path}: {e}"))?;
    file.write_all(&bytes)
        .await
        .map_err(|e| format!("write: {e}"))?;
    file.flush().await.ok();
    file.shutdown().await.ok();
    drop(file);
    let _ = sftp.close().await;
    crate::dlog(&format!(
        "[drop] uploaded {} bytes to {} (ws={}, pane={})",
        bytes.len(),
        remote_path,
        workspace_id,
        pane_id,
    ));
    // Resolve to absolute via ~ for caller readability; the shell will
    // expand the leading `~`.
    Ok(format!("~/{remote_path}"))
}

#[tauri::command]
pub(crate) async fn file_upload(
    state: State<'_, AppState>,
    workspace_id: String,
    local_path: String,
    remote_path: String,
) -> Result<u64, String> {
    let detail = format!("local={local_path} → remote={remote_path}");
    fm_log(
        "upload",
        &detail,
        async {
            let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
                .ok_or_else(|| "no active SSH session".to_string())?;
            let local = PathBuf::from(expand_path(&local_path));
            let bytes = std::fs::read(&local).map_err(|e| format!("read {local:?}: {e}"))?;
            let sftp = open_sftp(&handle).await?;
            let mut file = sftp
                .create(&remote_path)
                .await
                .map_err(|e| format!("sftp create {remote_path}: {e}"))?;
            file.write_all(&bytes)
                .await
                .map_err(|e| format!("write: {e}"))?;
            file.flush().await.ok();
            file.shutdown().await.ok();
            let n = bytes.len() as u64;
            drop(file);
            let _ = sftp.close().await;
            Ok(n)
        }
        .await,
    )
}

#[tauri::command]
pub(crate) async fn file_download(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    remote_path: String,
    local_path: String,
) -> Result<u64, String> {
    let detail = format!("remote={remote_path} → local={local_path}");
    fm_log(
        "download",
        &detail,
        async {
            let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
                .ok_or_else(|| "no active SSH session".to_string())?;
            let local = PathBuf::from(expand_path(&local_path));
            if let Some(parent) = local.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir parent {parent:?}: {e}"))?;
                }
            }
            let sftp = open_sftp(&handle).await?;
            let n = stream_download(&app, &sftp, &remote_path, &local).await?;
            let _ = sftp.close().await;
            Ok(n)
        }
        .await,
    )
}

/// Phase 62.B (item J): download a file referenced by an OSC 8 hyperlink
/// in the terminal. Claude Code (and other tools) emit
/// `\e]8;;file:///<absolute remote path>\e\\name\e]8;;\e\\`; the frontend
/// terminal linkHandler extracts the path from the `file://` URI and
/// calls this. SFTP-pulls the remote path into the user's Downloads
/// folder and returns the local destination (shown in a toast).
#[tauri::command]
pub(crate) async fn download_remote_file_via_osc(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    remote_path: String,
) -> Result<String, String> {
    fm_log(
        "download-osc",
        &format!("remote={remote_path}"),
        async {
            let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
                .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
            // Basename of a POSIX remote path; fall back to a generic name.
            let base = remote_path
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("download")
                .to_string();
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map_err(|_| "cannot resolve home directory".to_string())?;
            let downloads = std::path::Path::new(&home).join("Downloads");
            std::fs::create_dir_all(&downloads).map_err(|e| format!("mkdir Downloads: {e}"))?;
            let local = downloads.join(&base);
            let sftp = open_sftp(&handle).await?;
            stream_download(&app, &sftp, &remote_path, &local).await?;
            let _ = sftp.close().await;
            Ok(local.to_string_lossy().to_string())
        }
        .await,
    )
}

// ─── Phase 17: Open with OS default app ────────────────────────────────────

/// Spawn the Windows shell `start` to launch a file with whatever app
/// is registered as the default handler for the file's extension.
/// Equivalent of the user double-clicking the file in Explorer.
///
/// We deliberately use `cmd /C start "" "<path>"` rather than
/// `tauri::api::shell::open` so the call doesn't require the user's
/// Tauri shell-allowlist scope to cover every path under
/// `%USERPROFILE%`. `start` with an empty title argument (the `""` after
/// `start`) is the historic Windows incantation for "open this path
/// via shell association" — it handles paths with spaces and the
/// `\\?\` long-path prefix.
fn shell_open(path: &std::path::Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        // /C → run command then exit. Empty quotes are the START
        // "title" argument — required when the path is quoted so
        // start doesn't interpret it as the title.
        let status = std::process::Command::new("cmd")
            .args(["/C", "start", "", path_str.as_str()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| format!("spawn cmd: {e}"))?;
        if !status.success() {
            return Err(format!("cmd /C start exited {status}"));
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(&path_str)
            .status()
            .map_err(|e| format!("spawn open: {e}"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&path_str)
            .status()
            .map_err(|e| format!("spawn xdg-open: {e}"))?;
        Ok(())
    }
}

#[tauri::command]
pub(crate) fn file_open_local(path: String) -> Result<(), String> {
    let p = PathBuf::from(expand_path(&path));
    if !p.exists() {
        return Err(format!("file not found: {path}"));
    }
    shell_open(&p)
}

/// Build a stable temp path for a downloaded remote file. We bucket
/// by workspace_id so multiple servers don't clobber each other's
/// `package.json`, and reuse the same path on repeated opens so the
/// user keeps editing the same staging file.
fn remote_temp_path(workspace_id: &str, remote_path: &str) -> PathBuf {
    let basename = std::path::Path::new(remote_path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "winmux-download".to_string());
    let mut p = std::env::temp_dir();
    p.push("winmux");
    p.push(workspace_id);
    p.push(basename);
    p
}

// ─── Phase 17.B: Read / write (built-in editor) ────────────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct FileContents {
    pub text: String,
    pub encoding: &'static str,
    pub is_binary: bool,
    pub size: u64,
    /// True when our text-vs-binary heuristic ruled the file binary
    /// but we'd already populated `text` with the lossy-UTF-8 read
    /// for previewing. The frontend uses this to gray out Save.
    pub truncated: bool,
}

/// Heuristic: is this a text file we can safely edit?
/// 1. Extension whitelist (covers the common cases that the user
///    actually wants to edit — code, config, scripts, plain text).
/// 2. First 8 KB byte check: if the slice has any NUL bytes OR more
///    than 5% of bytes lie outside the printable / common-whitespace
///    range, treat as binary.
fn is_text_file(path: &str, head: &[u8]) -> bool {
    let lower = path.to_lowercase();
    const TEXT_EXTS: &[&str] = &[
        ".txt", ".md", ".markdown", ".rst", ".log",
        ".json", ".yaml", ".yml", ".toml", ".xml", ".ini", ".conf", ".cfg",
        ".env", ".gitignore", ".gitattributes", ".editorconfig", ".dockerignore",
        ".py", ".pyw", ".rb", ".pl", ".lua", ".php",
        ".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs",
        ".html", ".htm", ".css", ".scss", ".sass", ".less", ".svg",
        ".rs", ".go", ".java", ".kt", ".swift", ".scala", ".cs", ".vb", ".fs",
        ".c", ".cpp", ".cc", ".cxx", ".h", ".hpp", ".hxx",
        ".sh", ".bash", ".zsh", ".fish", ".ps1", ".psm1", ".bat", ".cmd",
        ".sql", ".prisma", ".graphql", ".proto",
        ".dockerfile", ".makefile", ".cmake",
        ".lock", ".gradle", ".sbt",
        ".tex", ".bib",
    ];
    if TEXT_EXTS.iter().any(|e| lower.ends_with(e)) {
        return true;
    }
    // Some text files have no extension at all (Dockerfile, Makefile,
    // README). Check magic-bytes style: if the byte sample is mostly
    // printable, treat as text.
    if head.is_empty() {
        // Empty file — assume text.
        return true;
    }
    let mut bad = 0usize;
    let mut nul = false;
    for &b in head {
        if b == 0 {
            nul = true;
            break;
        }
        let printable = b == 0x09 || b == 0x0a || b == 0x0d || (0x20..=0x7e).contains(&b);
        if !printable && b < 0x80 {
            bad += 1;
        }
    }
    if nul {
        return false;
    }
    // < 5% non-printable ASCII → text.
    bad * 20 < head.len()
}

const LARGE_FILE_THRESHOLD: u64 = 1 * 1024 * 1024;

fn classify_bytes(path: &str, bytes: &[u8], size: u64) -> FileContents {
    let head_len = bytes.len().min(8 * 1024);
    let head = &bytes[..head_len];
    let text_ish = is_text_file(path, head);
    if !text_ish {
        return FileContents {
            text: String::new(),
            encoding: "binary",
            is_binary: true,
            size,
            truncated: true,
        };
    }
    // Try strict UTF-8 first; fall back to lossy if the file is
    // mostly text but contains some non-UTF-8 bytes (windows-1252
    // configs, etc.).
    let (text, encoding) = match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf-8"),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), "lossy-utf-8"),
    };
    FileContents {
        text,
        encoding,
        is_binary: false,
        size,
        truncated: false,
    }
}

#[tauri::command]
pub(crate) fn file_read_local(path: String) -> Result<FileContents, String> {
    let p = PathBuf::from(expand_path(&path));
    let bytes = std::fs::read(&p).map_err(|e| format!("read {p:?}: {e}"))?;
    let size = bytes.len() as u64;
    Ok(classify_bytes(&path, &bytes, size))
}

#[tauri::command]
pub(crate) fn file_write_local(path: String, text: String) -> Result<(), String> {
    let p = PathBuf::from(expand_path(&path));
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent: {e}"))?;
        }
    }
    std::fs::write(&p, text).map_err(|e| format!("write {p:?}: {e}"))
}

#[tauri::command]
pub(crate) async fn file_read_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
) -> Result<FileContents, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let sftp = open_sftp(&handle).await?;
    let mut file = sftp
        .open(&path)
        .await
        .map_err(|e| format!("sftp open {path}: {e}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("read: {e}"))?;
    drop(file);
    let _ = sftp.close().await;
    let size = bytes.len() as u64;
    Ok(classify_bytes(&path, &bytes, size))
}

#[tauri::command]
pub(crate) async fn file_write_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
    text: String,
) -> Result<u64, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let bytes = text.into_bytes();
    let sftp = open_sftp(&handle).await?;
    let mut file = sftp
        .create(&path)
        .await
        .map_err(|e| format!("sftp create {path}: {e}"))?;
    file.write_all(&bytes).await.map_err(|e| format!("write: {e}"))?;
    file.flush().await.ok();
    file.shutdown().await.ok();
    let n = bytes.len() as u64;
    drop(file);
    let _ = sftp.close().await;
    Ok(n)
}

/// Surface the large-file threshold so the frontend can warn before
/// kicking off a read. Keeps the constant in one place.
#[tauri::command]
pub(crate) fn file_large_threshold() -> u64 {
    LARGE_FILE_THRESHOLD
}

#[tauri::command]
pub(crate) async fn file_open_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    remote_path: String,
) -> Result<String, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let dest = remote_temp_path(&workspace_id, &remote_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir temp parent {parent:?}: {e}"))?;
    }
    // SFTP download — same as file_download, but the destination is
    // resolved internally.
    let sftp = open_sftp(&handle).await?;
    let mut file = sftp
        .open(&remote_path)
        .await
        .map_err(|e| format!("sftp open {remote_path}: {e}"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .await
        .map_err(|e| format!("read: {e}"))?;
    drop(file);
    let _ = sftp.close().await;
    std::fs::write(&dest, &buf).map_err(|e| format!("write {dest:?}: {e}"))?;
    shell_open(&dest)?;
    Ok(dest.to_string_lossy().to_string())
}

// ─── Phase 57: zip / unzip ──────────────────────────────────────────

use std::io::{Read, Write};

/// Recursively walk a directory and add every regular file under it to
/// the open zip writer. Paths inside the archive are stored relative
/// to `arc_base` so that unpacking reproduces the same tree.
fn zip_add_dir(
    zw: &mut zip::ZipWriter<std::fs::File>,
    fs_root: &std::path::Path,
    arc_base: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    // Mark the directory itself (so empty directories round-trip).
    zw.add_directory(arc_base, opts).map_err(|e| e.to_string())?;
    let read = std::fs::read_dir(fs_root).map_err(|e| format!("read_dir {fs_root:?}: {e}"))?;
    for ent in read.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().to_string();
        let arc_name = if arc_base.is_empty() {
            name.clone()
        } else {
            format!("{arc_base}/{name}")
        };
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            zip_add_dir(zw, &path, &arc_name, opts)?;
        } else if md.is_file() {
            zw.start_file(&arc_name, opts).map_err(|e| e.to_string())?;
            let mut f = std::fs::File::open(&path)
                .map_err(|e| format!("open {path:?}: {e}"))?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(|e| format!("read {path:?}: {e}"))?;
                if n == 0 {
                    break;
                }
                zw.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            }
        }
        // Symlinks + special files: skipped (zip can store them but the
        // common-case .zip Yossi will produce/consume is plain files +
        // dirs; the existing dual-column FM also handles those poorly).
    }
    Ok(())
}

/// Phase 57: compress one or more local items in `cwd` into a single
/// `<cwd>/<output_name>` zip. `paths` are basenames relative to `cwd`
/// (the FE pulls them from the selected rows on the local column).
/// Returns the absolute path of the produced zip.
#[tauri::command]
pub(crate) async fn file_manager_zip_local(
    cwd: String,
    paths: Vec<String>,
    output_name: String,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("zip: no items selected".into());
    }
    if output_name.contains('/') || output_name.contains('\\') {
        return Err("zip: output_name must be a basename, not a path".into());
    }
    let cwd_pb = std::path::PathBuf::from(expand_path(&cwd));
    let out_pb = cwd_pb.join(&output_name);
    // Run the actual zip walk on a blocking thread — large trees would
    // otherwise stall the tokio worker.
    let cwd_clone = cwd_pb.clone();
    let out_clone = out_pb.clone();
    let paths_clone = paths.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::create(&out_clone)
            .map_err(|e| format!("create {out_clone:?}: {e}"))?;
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for p in &paths_clone {
            let src = cwd_clone.join(p);
            let arc_name = p.clone();
            let md = std::fs::symlink_metadata(&src)
                .map_err(|e| format!("stat {src:?}: {e}"))?;
            if md.is_dir() {
                zip_add_dir(&mut zw, &src, &arc_name, opts)?;
            } else if md.is_file() {
                zw.start_file(&arc_name, opts).map_err(|e| e.to_string())?;
                let mut f = std::fs::File::open(&src)
                    .map_err(|e| format!("open {src:?}: {e}"))?;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = f.read(&mut buf).map_err(|e| format!("read {src:?}: {e}"))?;
                    if n == 0 {
                        break;
                    }
                    zw.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                }
            }
        }
        zw.finish().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;
    Ok(out_pb.to_string_lossy().to_string())
}

/// Phase 60 (smoke-test 3b): pre-flight for the local unzip — does
/// the destination directory already exist with content? The FE asks
/// before extraction and shows an overwrite confirmation when true.
/// An existing-but-EMPTY dir doesn't count as a conflict (nothing to
/// lose).
#[tauri::command]
pub(crate) fn file_manager_unzip_local_check(zip_path: String) -> Result<bool, String> {
    let zip_pb = std::path::PathBuf::from(expand_path(&zip_path));
    let parent = zip_pb
        .parent()
        .ok_or_else(|| "unzip check: zip_path has no parent".to_string())?;
    let stem = zip_pb
        .file_stem()
        .ok_or_else(|| "unzip check: zip_path has no file stem".to_string())?;
    let dest = parent.join(stem);
    if !dest.exists() {
        return Ok(false);
    }
    let non_empty = std::fs::read_dir(&dest)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false);
    Ok(non_empty)
}

/// Phase 60 (smoke-test 3b): remote-side pre-flight. `[ -e <dest> ]`
/// over the workspace's SSH handle; exit 0 = exists. We don't probe
/// emptiness remotely — a one-liner `ls -A | head` adds a quoting
/// surface for marginal value; "the folder exists" is warning enough.
#[tauri::command]
pub(crate) async fn file_manager_unzip_remote_check(
    state: State<'_, AppState>,
    workspace_id: String,
    zip_path: String,
) -> Result<bool, String> {
    let zp = std::path::Path::new(&zip_path);
    let parent_dir = zp
        .parent()
        .and_then(|p| p.to_str())
        .ok_or_else(|| "unzip check: zip_path has no parent".to_string())?
        .to_string();
    let stem = zp
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "unzip check: zip_path has no file stem".to_string())?
        .to_string();
    let dest = if parent_dir.is_empty() {
        stem
    } else {
        format!("{parent_dir}/{stem}")
    };
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| {
            "no active SSH session for this workspace — connect a terminal pane first"
                .to_string()
        })?;
    let cmd = format!("test -e {}", winmux_core::shell_quote(&dest));
    let (_out, code) = remote_exec(&handle, &cmd).await?;
    Ok(code == 0)
}

/// Phase 57: extract `zip_path` into `<dirname(zip_path)>/<basename(zip_path)
/// without .zip>/`. Returns the destination directory's path.
#[tauri::command]
pub(crate) async fn file_manager_unzip_local(
    zip_path: String,
) -> Result<String, String> {
    let zip_pb = std::path::PathBuf::from(expand_path(&zip_path));
    let parent = zip_pb
        .parent()
        .ok_or_else(|| "unzip: zip_path has no parent".to_string())?
        .to_path_buf();
    let stem = zip_pb
        .file_stem()
        .ok_or_else(|| "unzip: zip_path has no file stem".to_string())?
        .to_string_lossy()
        .to_string();
    let dest = parent.join(&stem);
    std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir {dest:?}: {e}"))?;
    let dest_clone = dest.clone();
    let zip_clone = zip_pb.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::open(&zip_clone)
            .map_err(|e| format!("open {zip_clone:?}: {e}"))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("open zip archive: {e}"))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("zip entry {i}: {e}"))?;
            // Defense-in-depth against zip-slip: refuse any entry whose
            // normalized path tries to escape the destination dir.
            let raw_name = entry.name().to_string();
            let safe = match entry.enclosed_name() {
                Some(p) => p.to_path_buf(),
                None => {
                    return Err(format!(
                        "unzip: refusing unsafe path in archive: {raw_name:?}"
                    ));
                }
            };
            let out_path = dest_clone.join(&safe);
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)
                    .map_err(|e| format!("mkdir {out_path:?}: {e}"))?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {parent:?}: {e}"))?;
                }
                let mut out = std::fs::File::create(&out_path)
                    .map_err(|e| format!("create {out_path:?}: {e}"))?;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = entry.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;
    Ok(dest.to_string_lossy().to_string())
}

/// Shared helper for the two remote subcommand-style entrypoints —
/// runs a `bash -lc <command>` on the workspace's existing SSH handle
/// and waits for it to exit. Returns the joined stdout+stderr text
/// and the exit code. Caller decides how to surface non-zero codes.
async fn remote_exec(
    handle: &SshHandle<SshClient>,
    command: &str,
) -> Result<(String, i32), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.exec(true, command)
        .await
        .map_err(|e| format!("exec: {e}"))?;
    let mut out_buf = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(russh::ChannelMsg::Data { data }) => out_buf.extend_from_slice(&data[..]),
            Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                out_buf.extend_from_slice(&data[..])
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = exit_status as i32
            }
            Some(russh::ChannelMsg::Close)
            | Some(russh::ChannelMsg::Eof)
            | None => break,
            _ => {}
        }
    }
    Ok((String::from_utf8_lossy(&out_buf).to_string(), exit_code))
}

/// Phase 57: zip on the remote side. Requires the standard `zip`
/// binary on the remote (Debian/Ubuntu/Fedora all ship it). The
/// remote command does `cd <cwd> && zip -r <out> <items>...`. Every
/// caller-supplied string is shell-quoted via winmux_core::shell_quote
/// per Absolute Rule #3 — no string concatenation of user input.
#[tauri::command]
pub(crate) async fn file_manager_zip_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    cwd: String,
    paths: Vec<String>,
    output_name: String,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("zip: no items selected".into());
    }
    if output_name.contains('/') || output_name.contains('\\') {
        return Err("zip: output_name must be a basename, not a path".into());
    }
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| {
            "no active SSH session for this workspace — connect a terminal pane first"
                .to_string()
        })?;
    let mut parts: Vec<String> = Vec::with_capacity(paths.len() + 4);
    parts.push("cd".into());
    parts.push(winmux_core::shell_quote(&cwd));
    parts.push("&& zip -r".into());
    parts.push(winmux_core::shell_quote(&output_name));
    for p in &paths {
        parts.push(winmux_core::shell_quote(p));
    }
    let cmd = parts.join(" ");
    let (out, code) = remote_exec(&handle, &cmd).await?;
    if code != 0 {
        // Phase 65 (bug 2.5): the failure was previously invisible in
        // debug.log. Log metadata (workspace, output, exit code, stderr)
        // so "zip: command not found" (exit 127) is diagnosable. The
        // frontend turns 127 into a tar-fallback offer.
        crate::dlog(&format!(
            "file_manager_zip_remote FAILED ws={workspace_id} out={output_name} exit={code}: {out}"
        ));
        return Err(format!("remote zip failed (exit {code}): {out}"));
    }
    // The produced archive lives at cwd/output_name on the remote.
    Ok(format!("{}/{}", cwd.trim_end_matches('/'), output_name))
}

/// Phase 65 (bug 2.5): tar.gz fallback for servers without `zip`. Mirrors
/// `file_manager_zip_remote` but runs `cd <cwd> && tar -czf <out> <items>`.
/// `tar` is part of coreutils/busybox and is present on essentially every
/// Linux box, so this is the graceful path when the zip offer is declined
/// or zip is missing. Every string is shell-quoted (Absolute Rule #3).
#[tauri::command]
pub(crate) async fn file_manager_targz_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    cwd: String,
    paths: Vec<String>,
    output_name: String,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("tar: no items selected".into());
    }
    if output_name.contains('/') || output_name.contains('\\') {
        return Err("tar: output_name must be a basename, not a path".into());
    }
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| {
            "no active SSH session for this workspace — connect a terminal pane first"
                .to_string()
        })?;
    let mut parts: Vec<String> = Vec::with_capacity(paths.len() + 4);
    parts.push("cd".into());
    parts.push(winmux_core::shell_quote(&cwd));
    parts.push("&& tar -czf".into());
    parts.push(winmux_core::shell_quote(&output_name));
    for p in &paths {
        parts.push(winmux_core::shell_quote(p));
    }
    let cmd = parts.join(" ");
    let (out, code) = remote_exec(&handle, &cmd).await?;
    if code != 0 {
        crate::dlog(&format!(
            "file_manager_targz_remote FAILED ws={workspace_id} out={output_name} exit={code}: {out}"
        ));
        return Err(format!("remote tar failed (exit {code}): {out}"));
    }
    Ok(format!("{}/{}", cwd.trim_end_matches('/'), output_name))
}

/// Phase 57: unzip on the remote side. Requires `unzip` on the remote.
/// Extracts into `<parent_of_zip>/<stem>/` so the layout matches the
/// local helper.
#[tauri::command]
pub(crate) async fn file_manager_unzip_remote(
    state: State<'_, AppState>,
    workspace_id: String,
    zip_path: String,
) -> Result<String, String> {
    let zp = std::path::Path::new(&zip_path);
    let parent_dir = zp
        .parent()
        .and_then(|p| p.to_str())
        .ok_or_else(|| "unzip: zip_path has no parent".to_string())?
        .to_string();
    let stem = zp
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "unzip: zip_path has no file stem".to_string())?
        .to_string();
    let dest = if parent_dir.is_empty() {
        stem.clone()
    } else {
        format!("{parent_dir}/{stem}")
    };
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| {
            "no active SSH session for this workspace — connect a terminal pane first"
                .to_string()
        })?;
    let cmd = format!(
        "mkdir -p {dest} && unzip -o {zip} -d {dest}",
        dest = winmux_core::shell_quote(&dest),
        zip = winmux_core::shell_quote(&zip_path),
    );
    let (out, code) = remote_exec(&handle, &cmd).await?;
    if code != 0 {
        return Err(format!("remote unzip failed (exit {code}): {out}"));
    }
    Ok(dest)
}
