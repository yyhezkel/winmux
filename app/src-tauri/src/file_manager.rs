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
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{AppState, Session, SshClient};

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
    let sessions = state.sessions.lock().ok()?;
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
    result
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
    r
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
    r
}

#[tauri::command]
pub(crate) async fn file_upload(
    state: State<'_, AppState>,
    workspace_id: String,
    local_path: String,
    remote_path: String,
) -> Result<u64, String> {
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

#[tauri::command]
pub(crate) async fn file_download(
    state: State<'_, AppState>,
    workspace_id: String,
    remote_path: String,
    local_path: String,
) -> Result<u64, String> {
    let handle = pick_ssh_handle_for_workspace(&state, &workspace_id)
        .ok_or_else(|| "no active SSH session".to_string())?;
    let local = PathBuf::from(expand_path(&local_path));
    if let Some(parent) = local.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir parent {parent:?}: {e}"))?;
        }
    }
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
    std::fs::write(&local, &buf).map_err(|e| format!("write {local:?}: {e}"))?;
    let _ = sftp.close().await;
    Ok(buf.len() as u64)
}
