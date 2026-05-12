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
