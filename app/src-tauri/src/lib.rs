mod notes;
mod remote_bootstrap;
mod rpc_server;
mod tunnel;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use russh::client;
use russh::ChannelMsg;
use russh_keys::key::PrivateKeyWithHashAlg;
use russh_keys::{HashAlg, PrivateKey};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
static PANE_COUNTER: AtomicU64 = AtomicU64::new(0);
static SPLIT_COUNTER: AtomicU64 = AtomicU64::new(0);

// ─── Session types ───────────────────────────────────────────────────────────

enum Session {
    Local(LocalSession),
    Ssh(SshSession),
}

struct LocalSession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

struct SshSession {
    tx: tokio::sync::mpsc::UnboundedSender<SshCmd>,
}

#[derive(Debug)]
enum SshCmd {
    Data(Vec<u8>),
    Resize(u32, u32),
    Kill,
}

type SessionMap = Arc<Mutex<HashMap<String, Session>>>;
type PaneSessionMap = Arc<Mutex<HashMap<String, String>>>;
type WorkspacesState = Arc<Mutex<WorkspacesFile>>;

/// Tri-state for whether persistence is safe:
/// - `Loaded`: load_from_disk succeeded (file present or absent doesn't matter — state reflects truth).
/// - `Failed`: load_from_disk hit a real error (read or parse). Persisting would clobber data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadState {
    Loaded,
    Failed,
}

#[derive(Clone, Serialize)]
pub(crate) struct NotificationItem {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) timestamp_ms: u128,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeedItemState {
    Pending,
    Allowed,
    Denied,
    Timedout,
    Passive,
}

#[derive(Clone, Serialize)]
pub(crate) struct FeedItem {
    pub(crate) request_id: String,
    pub(crate) kind: String,
    pub(crate) subkind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_id: Option<String>,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) payload: serde_json::Value,
    pub(crate) state: FeedItemState,
    pub(crate) created_ms: u128,
    pub(crate) blocking: bool,
}

#[derive(Default)]
pub(crate) struct FeedStore {
    pub(crate) items: std::collections::VecDeque<FeedItem>,
    pub(crate) pending: HashMap<String, tokio::sync::oneshot::Sender<String>>,
}

#[allow(dead_code)] // used as documentation; rpc_server has its own copy
const FEED_MAX_ITEMS: usize = 50;

#[derive(Default, Clone)]
pub(crate) struct AppState {
    pub(crate) sessions: SessionMap,
    pub(crate) pane_sessions: PaneSessionMap,
    pub(crate) workspaces: WorkspacesState,
    pub(crate) load_state: Arc<Mutex<Option<LoadState>>>,
    pub(crate) notifications: Arc<Mutex<Vec<NotificationItem>>>,
    pub(crate) pane_status: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) feed: Arc<Mutex<FeedStore>>,
    pub(crate) notes: Arc<Mutex<notes::NotesFile>>,
}

pub(crate) static NOTIF_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize)]
struct PtyDataEvent {
    session_id: String,
    data: String,
}

#[derive(Clone, Serialize)]
struct PtyExitEvent {
    session_id: String,
    reason: Option<String>,
}

// ─── Workspace data model ────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Connection {
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
    },
    Ssh {
        host: String,
        user: String,
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_path: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum LayoutNode {
    Pane {
        pane_id: String,
        connection: Connection,
        // Phase 7.A: optional human-readable annotations on each leaf. Both fields
        // serialize-skip when None so existing workspaces.json files round-trip
        // unchanged until the user edits one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation: Option<String>,
    },
    Split {
        split_id: String,
        direction: SplitDirection,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
        ratio: f32,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EnvVar {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    // Legacy field — folded into layout on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) connection: Option<Connection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) layout: Option<LayoutNode>,
    // Phase 7.C: per-workspace shell automation. Sent into the spawned shell after a
    // small delay (so the shell has finished printing its banner). `env` is exported
    // first, then `setup_command` runs. `teardown_command` is sent right before
    // disconnect with a brief grace period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) setup_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) teardown_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) env: Vec<EnvVar>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct WorkspacesFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    active_workspace_id: Option<String>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
}

fn default_version() -> u32 {
    1
}

#[derive(Deserialize)]
pub(crate) struct CreateInput {
    pub(crate) name: String,
    pub(crate) connection: Connection,
    #[serde(default)]
    pub(crate) color: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) setup_command: Option<String>,
    #[serde(default)]
    pub(crate) teardown_command: Option<String>,
    #[serde(default)]
    pub(crate) env: Option<Vec<EnvVar>>,
}

// ─── ID helpers ──────────────────────────────────────────────────────────────

fn next_session_id() -> String {
    format!("s{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn new_pane_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = PANE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("p_{:x}_{:x}", t, n)
}

pub(crate) fn new_split_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = SPLIT_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sp_{:x}_{:x}", t, n)
}

pub(crate) fn new_workspace_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("w_{:x}", t)
}

// ─── Persistence ─────────────────────────────────────────────────────────────

fn config_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or_else(|| "no config dir available".to_string())?
        .join("winmux");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    Ok(dir)
}

/// Same as `config_dir` but visible to other modules.
pub(crate) fn config_dir_pub() -> Result<PathBuf, String> {
    config_dir()
}

fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("workspaces.json"))
}

pub(crate) fn dlog(msg: &str) {
    if let Ok(dir) = config_dir() {
        let p = dir.join("debug.log");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .and_then(|mut f| {
                use std::io::Write as _;
                writeln!(f, "[{ts}] {msg}")
            });
    }
}

fn save_to_disk(file: &WorkspacesFile) -> Result<(), String> {
    use std::io::Write as _;

    if file.workspaces.is_empty() && file.active_workspace_id.is_none() {
        dlog(&format!(
            "save_to_disk: writing empty state (workspaces=0). version={}",
            file.version
        ));
    }

    let path = config_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("workspaces.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {:?}: {e}", tmp))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync tmp: {e}"))?;
    }

    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    dlog(&format!(
        "save_to_disk: wrote {} bytes ({} workspaces) → {:?}",
        text.len(),
        file.workspaces.len(),
        path
    ));
    Ok(())
}

fn load_from_disk() -> Result<WorkspacesFile, String> {
    let path = config_path()?;
    dlog(&format!("load_from_disk: path={:?} exists={}", path, path.exists()));
    if !path.exists() {
        dlog("load_from_disk: file absent → fresh empty state (LoadState=Loaded)");
        return Ok(WorkspacesFile {
            version: 1,
            active_workspace_id: None,
            workspaces: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {:?}: {e}", path))?;
    dlog(&format!("load_from_disk: read {} bytes", text.len()));
    let mut file: WorkspacesFile = serde_json::from_str(&text)
        .map_err(|e| format!("parse {:?}: {e}", path))?;
    dlog(&format!(
        "load_from_disk: parsed OK, version={}, {} workspaces, active={:?}",
        file.version,
        file.workspaces.len(),
        file.active_workspace_id
    ));

    let mut migrated = false;
    for ws in file.workspaces.iter_mut() {
        if ws.layout.is_none() {
            let conn = ws.connection.take().unwrap_or(Connection::Local { shell: None });
            ws.layout = Some(LayoutNode::Pane {
                pane_id: new_pane_id(),
                connection: conn,
                title: None,
                annotation: None,
            });
            migrated = true;
        }
    }
    if migrated {
        dlog("load_from_disk: migration ran — saving migrated layout");
        match save_to_disk(&file) {
            Ok(()) => dlog("load_from_disk: migration save OK"),
            Err(e) => dlog(&format!("load_from_disk: migration save FAILED: {e}")),
        }
    }
    Ok(file)
}

pub(crate) fn persist(state: &AppState) -> Result<(), String> {
    // SAFETY GATE: do not persist if load failed. We'd clobber existing data with our
    // empty default state.
    let load_state = *state.load_state.lock().unwrap();
    match load_state {
        Some(LoadState::Loaded) => {}
        Some(LoadState::Failed) => {
            dlog("persist: REFUSING — load_state=Failed, would clobber existing data");
            return Err(
                "persistence disabled: workspaces.json failed to load earlier; \
                 fix the file and restart"
                    .into(),
            );
        }
        None => {
            dlog("persist: REFUSING — load_state=None (setup hasn't completed)");
            return Err("persistence not yet initialized".into());
        }
    }
    let file = state.workspaces.lock().unwrap().clone();
    save_to_disk(&file)
}

// ─── Tree operations ─────────────────────────────────────────────────────────

pub(crate) fn find_pane_connection(node: &LayoutNode, target: &str) -> Option<Connection> {
    match node {
        LayoutNode::Pane {
            pane_id, connection, ..
        } if pane_id == target => Some(connection.clone()),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_connection(first, target).or_else(|| find_pane_connection(second, target))
        }
    }
}

pub(crate) fn collect_panes(node: &LayoutNode, out: &mut Vec<String>) {
    match node {
        LayoutNode::Pane { pane_id, .. } => out.push(pane_id.clone()),
        LayoutNode::Split { first, second, .. } => {
            collect_panes(first, out);
            collect_panes(second, out);
        }
    }
}

fn split_pane_in(node: LayoutNode, target: &str, dir: SplitDirection) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Pane {
            pane_id,
            connection,
            title,
            annotation,
        } => {
            if pane_id == target {
                let new_pane = LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    connection: connection.clone(),
                    title: None,
                    annotation: None,
                };
                let original = LayoutNode::Pane {
                    pane_id,
                    connection,
                    title,
                    annotation,
                };
                (
                    LayoutNode::Split {
                        split_id: new_split_id(),
                        direction: dir,
                        first: Box::new(original),
                        second: Box::new(new_pane),
                        ratio: 0.5,
                    },
                    true,
                )
            } else {
                (
                    LayoutNode::Pane {
                        pane_id,
                        connection,
                        title,
                        annotation,
                    },
                    false,
                )
            }
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            let (new_first, found1) = split_pane_in(*first, target, dir.clone());
            if found1 {
                return (
                    LayoutNode::Split {
                        split_id,
                        direction,
                        first: Box::new(new_first),
                        second,
                        ratio,
                    },
                    true,
                );
            }
            let (new_second, found2) = split_pane_in(*second, target, dir);
            (
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                },
                found2,
            )
        }
    }
}

/// Returns (new_root_or_None, removed_pane_id_if_any).
/// new_root is None if the entire tree was just one pane and it was the target (caller
/// should ignore the request; can't close last pane).
fn close_pane_in(node: LayoutNode, target: &str) -> (Option<LayoutNode>, Option<String>) {
    match node {
        LayoutNode::Pane {
            pane_id,
            connection,
            title,
            annotation,
        } => {
            // Last pane — can't remove; return unchanged whether or not target matches.
            let _ = pane_id == target;
            (
                Some(LayoutNode::Pane {
                    pane_id,
                    connection,
                    title,
                    annotation,
                }),
                None,
            )
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            // Direct-leaf optimization: if either child is the target pane, collapse.
            if let LayoutNode::Pane { pane_id, .. } = first.as_ref() {
                if pane_id == target {
                    let removed = pane_id.clone();
                    return (Some(*second), Some(removed));
                }
            }
            if let LayoutNode::Pane { pane_id, .. } = second.as_ref() {
                if pane_id == target {
                    let removed = pane_id.clone();
                    return (Some(*first), Some(removed));
                }
            }
            // Recurse deeper.
            let (new_first_opt, removed1) = close_pane_in(*first, target);
            let new_first = new_first_opt.expect("non-leaf recursion preserves node");
            if removed1.is_some() {
                return (
                    Some(LayoutNode::Split {
                        split_id,
                        direction,
                        first: Box::new(new_first),
                        second,
                        ratio,
                    }),
                    removed1,
                );
            }
            let (new_second_opt, removed2) = close_pane_in(*second, target);
            let new_second = new_second_opt.expect("non-leaf recursion preserves node");
            (
                Some(LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                }),
                removed2,
            )
        }
    }
}

/// Phase 7.A: update title and/or annotation on a pane leaf. Each `Option<Option<…>>`
/// arg has three states: `None` = leave unchanged, `Some(None)` = clear,
/// `Some(Some(value))` = set.
pub(crate) fn update_pane_in(
    node: LayoutNode,
    target: &str,
    new_title: Option<Option<String>>,
    new_annotation: Option<Option<String>>,
) -> LayoutNode {
    match node {
        LayoutNode::Pane {
            pane_id,
            connection,
            title,
            annotation,
        } => {
            if pane_id == target {
                LayoutNode::Pane {
                    pane_id,
                    connection,
                    title: new_title.unwrap_or(title),
                    annotation: new_annotation.unwrap_or(annotation),
                }
            } else {
                LayoutNode::Pane {
                    pane_id,
                    connection,
                    title,
                    annotation,
                }
            }
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => LayoutNode::Split {
            split_id,
            direction,
            first: Box::new(update_pane_in(
                *first,
                target,
                new_title.clone(),
                new_annotation.clone(),
            )),
            second: Box::new(update_pane_in(
                *second,
                target,
                new_title,
                new_annotation,
            )),
            ratio,
        },
    }
}

/// Find the workspace_id whose layout contains the given pane_id. Used by RPC
/// callers (CLI on remote) that know only the pane_id.
pub(crate) fn find_workspace_for_pane(file: &WorkspacesFile, pane_id: &str) -> Option<String> {
    for ws in &file.workspaces {
        if let Some(layout) = &ws.layout {
            if find_pane_connection(layout, pane_id).is_some() {
                return Some(ws.id.clone());
            }
        }
    }
    None
}

fn set_split_ratio_in(node: LayoutNode, target: &str, new_ratio: f32) -> LayoutNode {
    match node {
        p @ LayoutNode::Pane { .. } => p,
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            if split_id == target {
                LayoutNode::Split {
                    split_id,
                    direction,
                    first,
                    second,
                    ratio: new_ratio.clamp(0.05, 0.95),
                }
            } else {
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(set_split_ratio_in(*first, target, new_ratio)),
                    second: Box::new(set_split_ratio_in(*second, target, new_ratio)),
                    ratio,
                }
            }
        }
    }
}

// ─── Helpers (PTY events) ────────────────────────────────────────────────────

/// Phase 7.C: shell flavor for env-var syntax + setup-command line ending.
#[derive(Clone, Copy, Debug)]
enum ShellKind {
    PowerShell,
    Cmd,
    Posix,
}

fn detect_shell_kind(cmd: &str) -> ShellKind {
    let lower = cmd.to_ascii_lowercase();
    let stem = std::path::Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&lower);
    match stem {
        "pwsh" | "powershell" => ShellKind::PowerShell,
        "cmd" => ShellKind::Cmd,
        _ => ShellKind::Posix,
    }
}

fn format_env_line(kind: ShellKind, key: &str, value: &str) -> String {
    match kind {
        ShellKind::PowerShell => {
            // Single-quote in PS doesn't expand variables; double-quote expands.
            // We use single quotes for predictable behavior.
            let escaped = value.replace('\'', "''");
            format!("$env:{} = '{}'", key, escaped)
        }
        ShellKind::Cmd => {
            // cmd's `set` takes raw value; backslash and quotes pass through.
            // Strip newlines defensively.
            let one_line = value.replace(['\n', '\r'], " ");
            format!("set {}={}", key, one_line)
        }
        ShellKind::Posix => {
            // Single-quoted POSIX literal; embedded `'` becomes `'\''`.
            let escaped = value.replace('\'', "'\\''");
            format!("export {}='{}'", key, escaped)
        }
    }
}

fn line_ending_for(_kind: ShellKind) -> &'static str {
    // ConPTY accepts both, but Cmd is happiest with \r and PowerShell with either.
    // Posix prefers \n; \r\n also works for it.
    "\r\n"
}

/// Phase 7.C: after the shell has had a moment to print its banner and prompt,
/// inject the workspace's `env` exports + `setup_command` as if the user typed them.
fn schedule_setup_injection(
    sessions: SessionMap,
    session_id: String,
    shell_kind: ShellKind,
    env: Vec<EnvVar>,
    setup_command: Option<String>,
) {
    let setup = setup_command.filter(|s| !s.is_empty());
    if env.is_empty() && setup.is_none() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let mut bytes: Vec<u8> = Vec::new();
        let eol = line_ending_for(shell_kind);
        for v in &env {
            bytes.extend_from_slice(format_env_line(shell_kind, &v.key, &v.value).as_bytes());
            bytes.extend_from_slice(eol.as_bytes());
        }
        if let Some(s) = setup {
            bytes.extend_from_slice(s.as_bytes());
            bytes.extend_from_slice(eol.as_bytes());
        }
        if bytes.is_empty() {
            return;
        }
        let mut sessions = sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(&session_id) {
            match s {
                Session::Local(l) => {
                    use std::io::Write as _;
                    let _ = l.writer.write_all(&bytes);
                    let _ = l.writer.flush();
                }
                Session::Ssh(ssh) => {
                    let _ = ssh.tx.send(SshCmd::Data(bytes));
                }
            }
        }
    });
}

fn pick_default_shell(requested: Option<String>) -> String {
    if let Some(s) = requested.filter(|s| !s.is_empty()) {
        return s;
    }
    let path_var = std::env::var("PATH").unwrap_or_default();
    for candidate in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(candidate).is_file() {
                return candidate.to_string();
            }
        }
    }
    "cmd.exe".into()
}

fn emit_data(app: &AppHandle, session_id: &str, bytes: &[u8], leftover: &mut Vec<u8>) {
    leftover.extend_from_slice(bytes);
    let valid_up_to = match std::str::from_utf8(leftover) {
        Ok(_) => leftover.len(),
        Err(e) => e.valid_up_to(),
    };
    if valid_up_to == 0 {
        return;
    }
    let s = std::str::from_utf8(&leftover[..valid_up_to])
        .unwrap()
        .to_string();
    leftover.drain(..valid_up_to);
    let _ = app.emit(
        "pty:data",
        PtyDataEvent {
            session_id: session_id.to_string(),
            data: s,
        },
    );
}

/// Emits a transient status text for a pane. Used by remote-bootstrap to surface
/// progress/errors. The frontend listens on `pane:status` events.
pub(crate) fn emit_pane_status_event(app: &AppHandle, pane_id: &str, text: &str) {
    let _ = app.emit(
        "pane:status",
        serde_json::json!({ "pane_id": pane_id, "text": text }),
    );
}

/// Spawns a tokio task that clears a pane's status text after `secs` seconds.
pub(crate) fn schedule_status_clear(app: AppHandle, pane_id: String, secs: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        emit_pane_status_event(&app, &pane_id, "");
    });
}

fn emit_exit(app: &AppHandle, session_id: &str, reason: Option<String>) {
    let _ = app.emit(
        "pty:exit",
        PtyExitEvent {
            session_id: session_id.to_string(),
            reason,
        },
    );
}

fn cleanup_session_maps(
    sessions: &SessionMap,
    pane_sessions: &PaneSessionMap,
    pane_id: &str,
    session_id: &str,
) {
    let _ = sessions.lock().unwrap().remove(session_id);
    let mut p = pane_sessions.lock().unwrap();
    if p.get(pane_id).map(|s| s.as_str()) == Some(session_id) {
        p.remove(pane_id);
    }
}

// ─── Local PTY spawn ─────────────────────────────────────────────────────────

fn spawn_local_pty(
    state: &AppState,
    pane_id: String,
    app: &AppHandle,
    shell: Option<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty failed: {e}"))?;

    let shell_cmd = pick_default_shell(shell);
    let mut cmd = CommandBuilder::new(&shell_cmd);
    if let Some(d) = cwd.as_deref() {
        if Path::new(d).is_dir() {
            cmd.cwd(d);
        }
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn {shell_cmd} failed: {e}"))?;
    drop(pair.slave);

    let killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone_reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take_writer failed: {e}"))?;

    let id = next_session_id();
    let id_for_thread = id.clone();
    let pane_for_thread = pane_id.clone();
    let app_for_thread = app.clone();
    let sessions_for_thread = state.sessions.clone();
    let pane_sessions_for_thread = state.pane_sessions.clone();
    thread::spawn(move || {
        let mut leftover: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => emit_data(&app_for_thread, &id_for_thread, &buf[..n], &mut leftover),
                Err(_) => break,
            }
        }
        let _ = child.wait();
        cleanup_session_maps(
            &sessions_for_thread,
            &pane_sessions_for_thread,
            &pane_for_thread,
            &id_for_thread,
        );
        emit_exit(&app_for_thread, &id_for_thread, None);
    });

    state.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Local(LocalSession {
            writer,
            master: pair.master,
            killer,
        }),
    );
    Ok(id)
}

// ─── Known-hosts (TOFU) ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct KnownHost {
    #[serde(rename = "type")]
    key_type: String,
    fingerprint: String,
    first_seen: String,
    last_seen: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct KnownHostsFile {
    #[serde(default)]
    hosts: HashMap<String, KnownHost>,
}

fn known_hosts_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("known_hosts.json"))
}

fn load_known_hosts() -> KnownHostsFile {
    if let Ok(p) = known_hosts_path() {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(f) = serde_json::from_str::<KnownHostsFile>(&text) {
                return f;
            }
        }
    }
    KnownHostsFile::default()
}

fn save_known_hosts(file: &KnownHostsFile) -> Result<(), String> {
    let path = known_hosts_path()?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Default, Clone, Debug)]
struct HostCheckOutcome {
    fingerprint: String,
    key_type: String,
    matched: bool,
    is_unknown: bool,
    mismatch_old: Option<String>,
}

// ─── SSH spawn ───────────────────────────────────────────────────────────────

pub(crate) struct SshClient {
    target: String,
    accept_unknown: bool,
    result: Arc<Mutex<HostCheckOutcome>>,
    /// If set, the handler accepts forwarded-tcpip channels and bridges them to the
    /// local Named Pipe RPC server after validating this token on the first line.
    tunnel_token: Option<Arc<String>>,
}

#[async_trait::async_trait]
impl client::Handler for SshClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let key_type = server_public_key.algorithm().as_str().to_string();
        let mut known = load_known_hosts();
        let mut outcome = HostCheckOutcome {
            fingerprint: fp.clone(),
            key_type: key_type.clone(),
            matched: false,
            is_unknown: false,
            mismatch_old: None,
        };
        let now = iso_now();
        let existing = known.hosts.get(&self.target).cloned();
        let accept = match existing {
            Some(entry) if entry.fingerprint == fp => {
                outcome.matched = true;
                if let Some(h) = known.hosts.get_mut(&self.target) {
                    h.last_seen = now;
                    let _ = save_known_hosts(&known);
                }
                true
            }
            Some(entry) => {
                outcome.mismatch_old = Some(entry.fingerprint);
                if self.accept_unknown {
                    // User explicitly said "replace" — overwrite the known_hosts entry.
                    known.hosts.insert(
                        self.target.clone(),
                        KnownHost {
                            key_type,
                            fingerprint: fp,
                            first_seen: now.clone(),
                            last_seen: now,
                        },
                    );
                    let _ = save_known_hosts(&known);
                    true
                } else {
                    false
                }
            }
            None => {
                outcome.is_unknown = true;
                if self.accept_unknown {
                    known.hosts.insert(
                        self.target.clone(),
                        KnownHost {
                            key_type,
                            fingerprint: fp,
                            first_seen: now.clone(),
                            last_seen: now,
                        },
                    );
                    let _ = save_known_hosts(&known);
                    true
                } else {
                    false
                }
            }
        };
        *self.result.lock().unwrap() = outcome;
        Ok(accept)
    }

    /// Phase 6.3: when the server forwards a connection back to us (via reverse
    /// tunnel `tcpip-forward`), bridge it to the local Named Pipe RPC server.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if let Some(token) = self.tunnel_token.clone() {
            tunnel::spawn_bridge(channel, token);
        } else {
            tracing::warn!("forwarded-tcpip channel arrived but no tunnel_token set; dropping");
        }
        Ok(())
    }
}

fn key_load_needs_passphrase(err: &str) -> bool {
    let s = err.to_lowercase();
    s.contains("encrypted")
        || s.contains("passphrase")
        || s.contains("pem")
        || s.contains("kdf")
        || s.contains("decrypt")
}

/// Wraps a `PrivateKey` for authentication. RSA keys get SHA-512; everything else uses None.
fn pkwh(key: PrivateKey) -> Result<PrivateKeyWithHashAlg, String> {
    let key = Arc::new(key);
    let hash_alg = if key.algorithm().is_rsa() {
        Some(HashAlg::Sha512)
    } else {
        None
    };
    PrivateKeyWithHashAlg::new(key, hash_alg).map_err(|e| e.to_string())
}

/// Try ssh-agent auth via OpenSSH-for-Windows agent and Pageant in turn.
/// Returns `Some(true)` if any identity authenticated; `Some(false)` if an agent was
/// found but no identity authenticated; `None` if no agent was reachable at all.
///
/// Both backends speak the OpenSSH agent protocol over a Windows named pipe — they
/// only differ in pipe name (`openssh-ssh-agent` vs `pageant`). We hit them through
/// `russh_keys::agent::client::AgentClient::connect_named_pipe`, which returns
/// `Result` cleanly. We deliberately do NOT use `connect_pageant()`, because the
/// `pageant-0.0.1` crate it uses internally has an `unwrap()` at
/// `pageant_impl.rs:64` that panics on benign Windows API return codes when
/// Pageant isn't running.
async fn try_agent_auth(
    handle: &mut client::Handle<SshClient>,
    user: &str,
) -> Option<bool> {
    let mut any_agent_seen = false;

    for (label, pipe_path) in [
        ("openssh-ssh-agent", r"\\.\pipe\openssh-ssh-agent"),
        ("pageant", r"\\.\pipe\pageant"),
    ] {
        let mut agent =
            match russh_keys::agent::client::AgentClient::connect_named_pipe(pipe_path).await {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!("{label} pipe ({pipe_path}) not reachable: {e}");
                    continue;
                }
            };
        let identities = match agent.request_identities().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::debug!("{label} request_identities: {e}");
                continue;
            }
        };
        if identities.is_empty() {
            continue;
        }
        any_agent_seen = true;
        tracing::info!("{label}: {} identit(y/ies) offered", identities.len());
        for id in identities {
            match handle.authenticate_publickey_with(user, id, &mut agent).await {
                Ok(true) => return Some(true),
                Ok(false) => continue,
                Err(e) => {
                    tracing::debug!("{label} auth attempt: {e}");
                    continue;
                }
            }
        }
    }

    if any_agent_seen {
        Some(false)
    } else {
        None
    }
}

async fn try_authenticate(
    handle: &mut client::Handle<SshClient>,
    user: &str,
    key_path: Option<&str>,
    key_passphrase: Option<&str>,
    password: Option<&str>,
) -> Result<bool, String> {
    // 1) ssh-agent (OpenSSH agent / Pageant via named pipe).
    if let Some(true) = try_agent_auth(handle, user).await {
        return Ok(true);
    }

    // 2) Explicit key file (with optional passphrase).
    if let Some(p) = key_path {
        match russh_keys::load_secret_key(p, key_passphrase) {
            Ok(key) => {
                let pkwh = pkwh(key)?;
                if handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    return Ok(true);
                }
            }
            Err(e) => {
                let s = e.to_string();
                if key_load_needs_passphrase(&s) {
                    if key_passphrase.is_none() {
                        return Err(format!("KEY_PASSPHRASE_REQUIRED:{}", p));
                    }
                    return Err(format!("KEY_PASSPHRASE_BAD:{}:{}", p, s));
                }
                return Err(format!("load key {p}: {s}"));
            }
        }
    }

    // 3) Default key paths (tried without passphrase; encrypted keys silently skipped).
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|e| e.to_string())?;
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let p = format!("{}/.ssh/{}", home, name);
        if !Path::new(&p).exists() {
            continue;
        }
        if let Ok(key) = russh_keys::load_secret_key(&p, None) {
            if let Ok(pkwh) = pkwh(key) {
                if handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    return Ok(true);
                }
            }
        }
    }

    // 4) Password (sent to remote, not key passphrase).
    if let Some(pw) = password {
        if handle
            .authenticate_password(user, pw)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Run `echo $HOME` over a fresh exec channel. Returns (stdout, exit_code).
async fn remote_get_home(
    handle: &mut client::Handle<SshClient>,
) -> Result<(String, i32), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, "echo $HOME").await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExitStatus { exit_status }) => code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    Ok((String::from_utf8_lossy(&out).to_string(), code))
}

async fn spawn_ssh(
    state: &AppState,
    pane_id: String,
    app: &AppHandle,
    host: String,
    user: String,
    port: u16,
    key_path: Option<String>,
    key_passphrase: Option<String>,
    password: Option<String>,
    accept_unknown_host: bool,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let config = Arc::new(client::Config::default());
    let target = format!("{}:{}", host, port);
    let outcome_arc = Arc::new(Mutex::new(HostCheckOutcome::default()));
    let token = Arc::new(tunnel::generate_token());
    let sh = SshClient {
        target: target.clone(),
        accept_unknown: accept_unknown_host,
        result: outcome_arc.clone(),
        tunnel_token: Some(token.clone()),
    };

    let connect_res = client::connect(config, (host.as_str(), port), sh).await;
    let outcome = outcome_arc.lock().unwrap().clone();

    let mut handle = match connect_res {
        Ok(h) => h,
        Err(e) => {
            if outcome.is_unknown && !outcome.matched {
                return Err(format!(
                    "UNKNOWN_HOST:{}:{}:{}",
                    target, outcome.key_type, outcome.fingerprint
                ));
            }
            if let Some(old) = outcome.mismatch_old {
                return Err(format!(
                    "HOST_KEY_MISMATCH:{}:{}:{}:{}",
                    target, outcome.key_type, old, outcome.fingerprint
                ));
            }
            return Err(format!("connect {target}: {e}"));
        }
    };

    let auth_ok = try_authenticate(
        &mut handle,
        &user,
        key_path.as_deref(),
        key_passphrase.as_deref(),
        password.as_deref(),
    )
    .await?;
    if !auth_ok {
        return Err("authentication failed (agent, key, and password all failed)".into());
    }

    // Phase 6.2: best-effort bootstrap of the winmux Linux binary on the remote.
    // We never block the user's shell on this — failures are surfaced via pane:status.
    emit_pane_status_event(app, &pane_id, "bootstrapping winmux…");
    match remote_bootstrap::bootstrap(&mut handle, app, false).await {
        Ok(remote_bootstrap::BootstrapStatus::AlreadyOk) => {
            emit_pane_status_event(app, &pane_id, "");
        }
        Ok(remote_bootstrap::BootstrapStatus::Uploaded { bytes, sha256: _ }) => {
            emit_pane_status_event(
                app,
                &pane_id,
                &format!("winmux installed ({} bytes)", bytes),
            );
            schedule_status_clear(app.clone(), pane_id.clone(), 3);
        }
        Ok(remote_bootstrap::BootstrapStatus::UnsupportedArch(arch)) => {
            emit_pane_status_event(
                app,
                &pane_id,
                &format!("remote arch '{}' not supported (no winmux binary)", arch),
            );
            schedule_status_clear(app.clone(), pane_id.clone(), 5);
        }
        Err(e) => {
            tracing::warn!("remote bootstrap failed: {e}");
            emit_pane_status_event(app, &pane_id, &format!("bootstrap failed: {e}"));
            schedule_status_clear(app.clone(), pane_id.clone(), 5);
        }
    }

    // Phase 6.3: ask server to forward a port back to us. With port=0 the server
    // picks a free one and returns it. Forwarded channels arrive in our Handler's
    // `server_channel_open_forwarded_tcpip` and get bridged to the local pipe.
    let remote_port = match handle.tcpip_forward("127.0.0.1", 0).await {
        Ok(p) => {
            dlog(&format!("tunnel: tcpip_forward got remote port {p}"));
            p
        }
        Err(e) => {
            dlog(&format!("tunnel: tcpip_forward failed: {e}"));
            tracing::warn!("tcpip_forward failed: {e}");
            0
        }
    };

    if remote_port != 0 {
        // Best-effort: write the env file so the CLI can dial back even if sshd
        // refuses our `set_env` requests on the shell channel.
        let (home_out, _) = match remote_get_home(&mut handle).await {
            Ok(v) => v,
            Err(e) => {
                dlog(&format!("tunnel: skip env-file write — couldn't read $HOME: {e}"));
                (String::new(), 1)
            }
        };
        let home = home_out.trim();
        if !home.is_empty() {
            let socket_addr = format!("127.0.0.1:{}", remote_port);
            if let Err(e) =
                tunnel::write_remote_env_file(&mut handle, home, &socket_addr, &token, &pane_id)
                    .await
            {
                dlog(&format!("tunnel: env-file write failed: {e}"));
            }
        }
    }

    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open_session: {e}"))?;

    // Best-effort: try to set env vars on the shell. sshd's AcceptEnv may filter; if so,
    // the env-file fallback covers it.
    if remote_port != 0 {
        let socket_addr = format!("127.0.0.1:{}", remote_port);
        let _ = channel.set_env(false, "WINMUX_SOCKET_ADDR", socket_addr).await;
        let _ = channel
            .set_env(false, "WINMUX_TUNNEL_TOKEN", token.as_str().to_string())
            .await;
        let _ = channel
            .set_env(false, "WINMUX_PANE_ID", pane_id.clone())
            .await;
    }

    channel
        .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await
        .map_err(|e| format!("request_pty: {e}"))?;
    channel
        .request_shell(true)
        .await
        .map_err(|e| format!("request_shell: {e}"))?;

    let id = next_session_id();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SshCmd>();

    let id_for_task = id.clone();
    let pane_for_task = pane_id.clone();
    let app_for_task = app.clone();
    let sessions_for_task = state.sessions.clone();
    let pane_sessions_for_task = state.pane_sessions.clone();
    tokio::spawn(async move {
        let mut leftover: Vec<u8> = Vec::new();
        let mut exit_reason: Option<String> = None;
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) => {
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut leftover);
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut leftover);
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            exit_reason = Some(format!("exit {exit_status}"));
                        }
                        Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => {
                            break;
                        }
                        _ => {}
                    }
                }
                cmd = rx.recv() => {
                    match cmd {
                        Some(SshCmd::Data(d)) => {
                            if channel.data(&d[..]).await.is_err() { break; }
                        }
                        Some(SshCmd::Resize(c, r)) => {
                            let _ = channel.window_change(c, r, 0, 0).await;
                        }
                        Some(SshCmd::Kill) | None => {
                            let _ = channel.close().await;
                            break;
                        }
                    }
                }
            }
        }
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        cleanup_session_maps(
            &sessions_for_task,
            &pane_sessions_for_task,
            &pane_for_task,
            &id_for_task,
        );
        emit_exit(&app_for_task, &id_for_task, exit_reason);
    });

    state
        .sessions
        .lock()
        .unwrap()
        .insert(id.clone(), Session::Ssh(SshSession { tx }));
    Ok(id)
}

pub(crate) fn kill_session_inner(s: &mut Session) {
    match s {
        Session::Local(l) => {
            let _ = l.killer.kill();
        }
        Session::Ssh(ssh) => {
            let _ = ssh.tx.send(SshCmd::Kill);
        }
    }
}

// ─── Workspace mutation commands ─────────────────────────────────────────────

#[tauri::command]
fn workspaces_load(state: State<'_, AppState>) -> Result<WorkspacesFile, String> {
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_create(
    state: State<'_, AppState>,
    input: CreateInput,
) -> Result<WorkspacesFile, String> {
    let ws = Workspace {
        id: new_workspace_id(),
        name: input.name,
        color: input.color,
        cwd: input.cwd,
        connection: None,
        layout: Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            connection: input.connection,
            title: None,
            annotation: None,
        }),
        setup_command: input.setup_command,
        teardown_command: input.teardown_command,
        env: input.env.unwrap_or_default(),
    };
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = Some(ws.id.clone());
        file.workspaces.push(ws);
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 7.C: edit a workspace's mutable metadata fields. Each field is `Option`:
/// `None` = don't touch; `Some(...)` = update. For `setup_command`/`teardown_command`/
/// `cwd`, an empty string is treated as "clear". For `env`, an empty Vec replaces
/// the whole list with empty.
#[tauri::command]
fn workspace_update(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    name: Option<String>,
    color: Option<String>,
    cwd: Option<String>,
    setup_command: Option<String>,
    teardown_command: Option<String>,
    env: Option<Vec<EnvVar>>,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        if let Some(n) = name {
            if !n.is_empty() {
                ws.name = n;
            }
        }
        if let Some(c) = color {
            ws.color = if c.is_empty() { None } else { Some(c) };
        }
        if let Some(d) = cwd {
            ws.cwd = if d.is_empty() { None } else { Some(d) };
        }
        if let Some(s) = setup_command {
            ws.setup_command = if s.is_empty() { None } else { Some(s) };
        }
        if let Some(t) = teardown_command {
            ws.teardown_command = if t.is_empty() { None } else { Some(t) };
        }
        if let Some(e) = env {
            ws.env = e;
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_rename(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.name = name;
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_delete(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    let panes_to_kill: Vec<String> = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.layout.as_ref())
            .map(|l| {
                let mut v = Vec::new();
                collect_panes(l, &mut v);
                v
            })
            .unwrap_or_default()
    };
    for pane_id in &panes_to_kill {
        if let Some(sid) = state.pane_sessions.lock().unwrap().remove(pane_id) {
            if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
                kill_session_inner(&mut s);
            }
        }
    }
    {
        let mut file = state.workspaces.lock().unwrap();
        file.workspaces.retain(|w| w.id != workspace_id);
        if file.active_workspace_id.as_deref() == Some(&workspace_id) {
            file.active_workspace_id = file.workspaces.first().map(|w| w.id.clone());
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_set_active(
    state: State<'_, AppState>,
    workspace_id: Option<String>,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        file.active_workspace_id = workspace_id;
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_split(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
    direction: SplitDirection,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                let (new_layout, _) = split_pane_in(layout, &pane_id, direction);
                ws.layout = Some(new_layout);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_close_pane(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
) -> Result<WorkspacesFile, String> {
    let removed_pane: Option<String>;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .take()
            .ok_or_else(|| "no layout".to_string())?;
        let (new_root, removed) = close_pane_in(layout, &pane_id);
        ws.layout = new_root;
        removed_pane = removed;
    }
    if let Some(pid) = removed_pane {
        if let Some(sid) = state.pane_sessions.lock().unwrap().remove(&pid) {
            if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
                kill_session_inner(&mut s);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn workspace_set_split_ratio(
    state: State<'_, AppState>,
    workspace_id: String,
    split_id: String,
    ratio: f32,
) -> Result<(), String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(set_split_ratio_in(layout, &split_id, ratio));
            }
        }
    }
    persist(&state)?;
    Ok(())
}

// ─── Pane metadata (title / annotation) ─────────────────────────────────────

#[tauri::command]
fn pane_set_title(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    title: Option<String>,
) -> Result<WorkspacesFile, String> {
    let normalized = title.filter(|s| !s.is_empty());
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_pane_in(layout, &pane_id, Some(normalized), None));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn pane_set_annotation(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    annotation: Option<String>,
) -> Result<WorkspacesFile, String> {
    let normalized = annotation.filter(|s| !s.is_empty());
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_pane_in(layout, &pane_id, None, Some(normalized)));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Pane connect / disconnect ───────────────────────────────────────────────

#[tauri::command]
async fn pane_connect(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    password: Option<String>,
    key_passphrase: Option<String>,
    accept_unknown_host: Option<bool>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    // Look up connection from workspaces state. Phase 7.C: also lift `env` and
    // `setup_command` from the workspace so we can inject them after the shell is up.
    let (conn, cwd, ws_env, ws_setup) = {
        let file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_ref()
            .ok_or_else(|| "no layout".to_string())?;
        let conn = find_pane_connection(layout, &pane_id)
            .ok_or_else(|| format!("no pane {pane_id}"))?;
        (
            conn,
            ws.cwd.clone(),
            ws.env.clone(),
            ws.setup_command.clone(),
        )
    };

    // Resolve shell kind for env-line formatting (need this BEFORE we move `conn`).
    let shell_kind = match &conn {
        Connection::Local { shell } => detect_shell_kind(&pick_default_shell(shell.clone())),
        Connection::Ssh { .. } => ShellKind::Posix,
    };

    // Kill any prior session for this pane.
    if let Some(old_sid) = state.pane_sessions.lock().unwrap().remove(&pane_id) {
        if let Some(mut s) = state.sessions.lock().unwrap().remove(&old_sid) {
            kill_session_inner(&mut s);
        }
    }

    let session_id = match conn {
        Connection::Local { shell } => {
            spawn_local_pty(&state, pane_id.clone(), &app, shell, cwd, cols, rows)?
        }
        Connection::Ssh {
            host,
            user,
            port,
            key_path,
        } => {
            spawn_ssh(
                &state,
                pane_id.clone(),
                &app,
                host,
                user,
                port,
                key_path,
                key_passphrase,
                password,
                accept_unknown_host.unwrap_or(false),
                cols,
                rows,
            )
            .await?
        }
    };
    state
        .pane_sessions
        .lock()
        .unwrap()
        .insert(pane_id, session_id.clone());

    // Phase 7.C: inject env exports + setup_command after a 500ms grace period.
    schedule_setup_injection(
        state.sessions.clone(),
        session_id.clone(),
        shell_kind,
        ws_env,
        ws_setup,
    );

    Ok(session_id)
}

#[tauri::command]
async fn pane_disconnect(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let sid = state.pane_sessions.lock().unwrap().remove(&pane_id);
    let Some(sid) = sid else {
        return Ok(());
    };

    // Phase 7.C: if the workspace has a teardown_command, send it and give the
    // shell ~500ms to run it before we drop the channel.
    let teardown = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| {
                w.layout
                    .as_ref()
                    .map(|l| find_pane_connection(l, &pane_id).is_some())
                    .unwrap_or(false)
            })
            .and_then(|w| w.teardown_command.clone())
            .filter(|s| !s.is_empty())
    };
    if let Some(t) = teardown {
        let bytes = format!("{}\r\n", t).into_bytes();
        {
            let mut sessions = state.sessions.lock().unwrap();
            if let Some(s) = sessions.get_mut(&sid) {
                match s {
                    Session::Local(l) => {
                        use std::io::Write as _;
                        let _ = l.writer.write_all(&bytes);
                        let _ = l.writer.flush();
                    }
                    Session::Ssh(ssh) => {
                        let _ = ssh.tx.send(SshCmd::Data(bytes));
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
        kill_session_inner(&mut s);
    }
    Ok(())
}

// ─── Session-level commands (write/resize) ───────────────────────────────────

pub(crate) fn write_to_session(state: &AppState, session_id: &str, data: &[u8]) -> Result<(), String> {
    let mut sessions = state.sessions.lock().unwrap();
    let s = sessions
        .get_mut(session_id)
        .ok_or_else(|| format!("no such session {session_id}"))?;
    match s {
        Session::Local(l) => {
            l.writer.write_all(data).map_err(|e| e.to_string())?;
            l.writer.flush().map_err(|e| e.to_string())?;
        }
        Session::Ssh(ssh) => {
            ssh.tx
                .send(SshCmd::Data(data.to_vec()))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn pty_write(state: State<'_, AppState>, session_id: String, data: String) -> Result<(), String> {
    write_to_session(&state, &session_id, data.as_bytes())
}

#[tauri::command]
fn notifications_list(state: State<'_, AppState>) -> Vec<NotificationItem> {
    state.notifications.lock().unwrap().clone()
}

#[tauri::command]
fn notifications_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.notifications.lock().unwrap().clear();
    Ok(())
}

#[tauri::command]
fn pane_status_get(state: State<'_, AppState>) -> HashMap<String, String> {
    state.pane_status.lock().unwrap().clone()
}

/// Phase 6.5: shared decision logic for feed items. Used both by the Tauri command
/// `feed_decide` (called by the frontend's Allow/Deny buttons) and by the RPC handler
/// when the timeout expires or sender drops.
pub(crate) fn decide_feed(
    state: &AppState,
    app: &AppHandle,
    request_id: &str,
    decision: &str,
) -> Result<(), String> {
    let new_state = match decision {
        "allow" => FeedItemState::Allowed,
        "deny" => FeedItemState::Denied,
        "timeout" => FeedItemState::Timedout,
        other => return Err(format!("unknown decision: {other}")),
    };
    let tx = {
        let mut store = state.feed.lock().unwrap();
        for item in store.items.iter_mut() {
            if item.request_id == request_id {
                item.state = new_state.clone();
            }
        }
        store.pending.remove(request_id)
    };
    let _ = app.emit(
        "feed:item-resolved",
        serde_json::json!({ "request_id": request_id, "decision": decision }),
    );
    if let Some(tx) = tx {
        let _ = tx.send(decision.to_string());
    }
    Ok(())
}

#[tauri::command]
fn feed_list(state: State<'_, AppState>) -> Vec<FeedItem> {
    state.feed.lock().unwrap().items.iter().cloned().collect()
}

#[tauri::command]
fn feed_decide(
    state: State<'_, AppState>,
    app: AppHandle,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    decide_feed(&state, &app, &request_id, &decision)
}

#[tauri::command]
fn pty_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state.sessions.lock().unwrap();
    let s = sessions
        .get(&session_id)
        .ok_or_else(|| format!("no such session {session_id}"))?;
    match s {
        Session::Local(l) => l
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string()),
        Session::Ssh(ssh) => ssh
            .tx
            .send(SshCmd::Resize(cols as u32, rows as u32))
            .map_err(|e| e.to_string()),
    }
}

// ─── Entrypoint ──────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            let state: State<AppState> = app.state();
            dlog("─── setup() starting ───");
            match load_from_disk() {
                Ok(file) => {
                    *state.workspaces.lock().unwrap() = file;
                    *state.load_state.lock().unwrap() = Some(LoadState::Loaded);
                    dlog("setup: load_state = Loaded");
                }
                Err(e) => {
                    *state.load_state.lock().unwrap() = Some(LoadState::Failed);
                    dlog(&format!(
                        "setup: load FAILED: {e} — load_state = Failed (persists will refuse)"
                    ));
                    tracing::warn!("workspaces load failed: {e}");
                }
            }
            // Phase 7.B: load notes (best-effort; missing file is fine).
            match notes::load_notes_from_disk() {
                Ok(nf) => {
                    let count = nf.notes.len();
                    *state.notes.lock().unwrap() = nf;
                    dlog(&format!("setup: notes loaded ({count} notes)"));
                }
                Err(e) => {
                    dlog(&format!("setup: notes load failed: {e} (starting empty)"));
                }
            }
            // Spawn JSON-RPC server on a per-user named pipe.
            let state_clone: AppState = (*state).clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                rpc_server::run(state_clone, app_handle).await;
            });
            dlog(&format!("setup: rpc server spawned on {}", rpc_server::pipe_name()));
            dlog("─── setup() done ───");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            workspaces_load,
            workspace_create,
            workspace_update,
            workspace_rename,
            workspace_delete,
            workspace_set_active,
            workspace_split,
            workspace_close_pane,
            workspace_set_split_ratio,
            pane_connect,
            pane_disconnect,
            pane_set_title,
            pane_set_annotation,
            pty_write,
            pty_resize,
            notifications_list,
            notifications_clear,
            pane_status_get,
            feed_list,
            feed_decide,
            notes::notes_load,
            notes::notes_add,
            notes::notes_update,
            notes::notes_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
