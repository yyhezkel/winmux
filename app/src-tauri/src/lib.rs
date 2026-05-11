mod connect_wizard;
mod dev;
mod notes;
mod provisioning;
mod remote_bootstrap;
mod rpc_server;
mod settings;
mod tunnel;
mod updater;

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

pub(crate) enum Session {
    Local(LocalSession),
    Ssh(SshSession),
}

pub(crate) struct LocalSession {
    pub(crate) writer: Box<dyn Write + Send>,
    pub(crate) master: Box<dyn MasterPty + Send>,
    pub(crate) killer: Box<dyn ChildKiller + Send + Sync>,
}

pub(crate) struct SshSession {
    pub(crate) tx: tokio::sync::mpsc::UnboundedSender<SshCmd>,
    // Phase 8.B: shared russh client handle. The I/O task and any port-forward
    // accept loop both hold an Arc; russh's Handle methods take &self, so
    // concurrent users send commands through the underlying mpsc sender.
    pub(crate) handle: Arc<client::Handle<SshClient>>,
    // Phase 8.B: workspace this session belongs to, so port-forward bookkeeping
    // can clean up when the workspace is deleted or all SSH sessions exit.
    pub(crate) workspace_id: String,
    // Phase 11.A: when this session was started with `persistent=true` we wrap
    // the shell in a tmux attach-or-create. Storing the name lets us send
    // `tmux kill-session -t NAME` via a separate exec channel on demand.
    pub(crate) tmux_session: Option<String>,
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

// Phase 8.B: SSH local port forwards (browser pane → remote dev server).
// Key = (workspace_id, remote_port). Value carries the local listener port and
// a oneshot to cancel the accept loop on cleanup.
pub(crate) struct ForwardEntry {
    pub(crate) local_port: u16,
    pub(crate) cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
pub(crate) type ForwardMap = Arc<Mutex<HashMap<(String, u16), ForwardEntry>>>;

// Phase 8.C: pending request → response map for browser-pane operations that
// need to round-trip through the frontend (eval, screenshot). Keyed by request_id.
pub(crate) type BrowserPending = Arc<
    Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>>,
>;
pub(crate) static BROWSER_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// Phase 8.C: pending `browser-wait` waiters keyed by pane_id. The frontend calls
// `pane_browser_loaded(pane_id, url)` on every iframe onload; that drains and
// resolves all pending waiters for that pane.
pub(crate) type BrowserLoadWaiters =
    Arc<Mutex<HashMap<String, Vec<tokio::sync::oneshot::Sender<String>>>>>;

// Phase 8.F.1: pending iframe-bridge requests, keyed by request_id. The parent
// webview's bridge forwards iframe responses back via `pane_browser_iframe_response`,
// which resolves the matching oneshot here.
pub(crate) type IframePending = Arc<
    Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>>,
>;
pub(crate) static IFRAME_REQ_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    // Phase 9.A: persistent app settings (theme, fonts, terminal, hooks, etc.)
    pub(crate) settings: Arc<Mutex<settings::Settings>>,
    // Phase 8.B: per-(workspace, remote_port) port forwards.
    pub(crate) forwards: ForwardMap,
    // Phase 8.C: pending browser requests (eval/screenshot) awaiting frontend reply.
    pub(crate) browser_pending: BrowserPending,
    // Phase 8.C: pending browser-wait waiters, drained on iframe onload.
    pub(crate) browser_load_waiters: BrowserLoadWaiters,
    // Phase 8.E: ring buffer of frontend console.error/warn captures.
    pub(crate) console_buffer: dev::ConsoleBuffer,
    // Phase 8.F.1: iframe-bridge pending requests.
    pub(crate) iframe_pending: IframePending,
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
pub(crate) enum SplitDirection {
    Horizontal,
    Vertical,
}

// Phase 8.A: pane kind. Defaults to Terminal so older workspaces.json (no `pane_kind`
// field) deserialize unchanged. `is_terminal_kind` lets serde elide the field on
// terminal panes, keeping legacy round-trips byte-identical.
#[derive(Clone, Copy, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PaneKind {
    #[default]
    Terminal,
    Browser,
}

fn is_terminal_kind(k: &PaneKind) -> bool {
    matches!(k, PaneKind::Terminal)
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct BrowserState {
    pub(crate) url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) home_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) history: Vec<String>,
    // Phase 8.B: when true (default) and the pane lives in an SSH workspace,
    // navigate-resolve rewrites localhost:N / 127.0.0.1:N to a forwarded local
    // listener. The address bar still shows the original URL.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub(crate) forward_localhost: bool,
    // Phase 8.C fix: URL the iframe most recently fired `load` for. Lets
    // `browser-wait` return immediately when the page is already loaded
    // instead of timing out waiting for the next load event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_loaded_url: Option<String>,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            url: String::new(),
            home_url: None,
            history: Vec::new(),
            forward_localhost: true,
            last_loaded_url: None,
        }
    }
}

fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

const BROWSER_HISTORY_MAX: usize = 50;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum LayoutNode {
    Pane {
        pane_id: String,
        // Phase 8.A: terminal vs browser. Default = Terminal so legacy JSON works.
        #[serde(default, skip_serializing_if = "is_terminal_kind")]
        pane_kind: PaneKind,
        // Phase 8.A: connection is required for terminal panes, absent for browser.
        // Legacy JSON always has it set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<Connection>,
        // Phase 8.A: browser pane state (url, home_url, history). None for terminal panes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser: Option<BrowserState>,
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
    // Override hook so users / tests can pin the state directory regardless of
    // how the Windows binary resolves it. If `WINMUX_CONFIG_DIR` is set we use
    // it verbatim; otherwise default to `dirs::config_dir()/winmux` which on
    // Windows resolves to `%APPDATA%\Roaming\winmux`.
    if let Ok(custom) = std::env::var("WINMUX_CONFIG_DIR") {
        let p = PathBuf::from(custom);
        std::fs::create_dir_all(&p).map_err(|e| format!("create {:?}: {e}", p))?;
        return Ok(p);
    }
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
                pane_kind: PaneKind::Terminal,
                connection: Some(conn),
                browser: None,
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

// Diagnostic: tag every persist with its caller so debug.log shows the exact
// Tauri/RPC handler that triggered each save. Helpful while chasing autosave
// loops; safe to remove once the regression is closed out.
#[track_caller]
pub(crate) fn persist(state: &AppState) -> Result<(), String> {
    let caller = std::panic::Location::caller();
    dlog(&format!(
        "persist: called from {}:{}",
        caller.file(),
        caller.line()
    ));
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
        } if pane_id == target => connection.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_connection(first, target).or_else(|| find_pane_connection(second, target))
        }
    }
}

// Phase 8.A: existence check independent of pane kind. find_pane_connection returns
// None for browser panes (no connection), so callers that only need "does this pane
// exist somewhere in this layout" must use this instead.
pub(crate) fn pane_id_exists_in(node: &LayoutNode, target: &str) -> bool {
    match node {
        LayoutNode::Pane { pane_id, .. } => pane_id == target,
        LayoutNode::Split { first, second, .. } => {
            pane_id_exists_in(first, target) || pane_id_exists_in(second, target)
        }
    }
}

// Phase 8.A: mutate a browser pane's state (or no-op if pane is terminal / not found).
pub(crate) fn update_browser_pane(
    node: LayoutNode,
    target: &str,
    f: &mut dyn FnMut(&mut BrowserState),
) -> LayoutNode {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            mut browser,
            title,
            annotation,
        } => {
            if pane_id == target && pane_kind == PaneKind::Browser {
                if let Some(b) = browser.as_mut() {
                    f(b);
                }
            }
            LayoutNode::Pane {
                pane_id,
                pane_kind,
                connection,
                browser,
                title,
                annotation,
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
            first: Box::new(update_browser_pane(*first, target, f)),
            second: Box::new(update_browser_pane(*second, target, f)),
            ratio,
        },
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

// Phase 8.E: visit every leaf pane and report its kind to the callback. Used
// by the `dev.get-state` summary builder.
pub(crate) fn collect_panes_with_kind(node: &LayoutNode, f: &mut dyn FnMut(PaneKind)) {
    match node {
        LayoutNode::Pane { pane_kind, .. } => f(*pane_kind),
        LayoutNode::Split { first, second, .. } => {
            collect_panes_with_kind(first, f);
            collect_panes_with_kind(second, f);
        }
    }
}

// Phase 8.A: `new_kind` decides whether the spawned sibling is a terminal (default,
// inherits the existing pane's connection) or a browser (with `new_browser_url` as
// the starting page).
pub(crate) fn split_pane_in(
    node: LayoutNode,
    target: &str,
    dir: SplitDirection,
    new_kind: PaneKind,
    new_browser_url: Option<String>,
) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            annotation,
        } => {
            if pane_id == target {
                let (new_kind_resolved, new_conn, new_browser) = match new_kind {
                    PaneKind::Terminal => {
                        // For a terminal sibling, inherit this pane's connection if it has
                        // one; otherwise (splitting off a browser pane) fall back to local.
                        let conn = connection
                            .clone()
                            .unwrap_or(Connection::Local { shell: None });
                        (PaneKind::Terminal, Some(conn), None)
                    }
                    PaneKind::Browser => {
                        let url = new_browser_url
                            .clone()
                            .unwrap_or_else(|| "about:blank".to_string());
                        let bs = BrowserState {
                            url: url.clone(),
                            home_url: Some(url),
                            history: Vec::new(),
                            forward_localhost: true,
                            last_loaded_url: None,
                        };
                        (PaneKind::Browser, None, Some(bs))
                    }
                };
                let new_pane = LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: new_kind_resolved,
                    connection: new_conn,
                    browser: new_browser,
                    title: None,
                    annotation: None,
                };
                let original = LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
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
                        pane_kind,
                        connection,
                        browser,
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
            let (new_first, found1) = split_pane_in(
                *first,
                target,
                dir.clone(),
                new_kind,
                new_browser_url.clone(),
            );
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
            let (new_second, found2) =
                split_pane_in(*second, target, dir, new_kind, new_browser_url);
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
            pane_kind,
            connection,
            browser,
            title,
            annotation,
        } => {
            // Last pane — can't remove; return unchanged whether or not target matches.
            let _ = pane_id == target;
            (
                Some(LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
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
            pane_kind,
            connection,
            browser,
            title,
            annotation,
        } => {
            if pane_id == target {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title: new_title.unwrap_or(title),
                    annotation: new_annotation.unwrap_or(annotation),
                }
            } else {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
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
            if pane_id_exists_in(layout, pane_id) {
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
/// Phase 11.A: tmux session names disallow `.` and `:` and (for sane shell
/// quoting) we also strip whitespace. Pane ids look like `p_<hex>_<n>`
/// already so this is a no-op in practice; the sanitizer is defensive
/// against future id format changes.
pub(crate) fn sanitize_tmux_session_name(pane_id: &str) -> String {
    let cleaned: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("winmux-{}", cleaned)
}

/// Minimal POSIX single-quote escape. Wraps the value in single quotes and
/// rewrites any internal single-quote as `'\''`. Safe for /bin/sh-style.
pub(crate) fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

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

impl SshClient {
    /// Construct a tolerant client for one-shot operations (the connect
    /// wizard test, provisioning steps). Accepts any server key,
    /// doesn't touch known_hosts, no tunnel token. The host-check
    /// outcome is captured but never persisted.
    pub(crate) fn new_anonymous(target: String) -> Self {
        Self {
            target,
            accept_unknown: true,
            result: Arc::new(Mutex::new(HostCheckOutcome {
                fingerprint: String::new(),
                key_type: String::new(),
                matched: true,
                is_unknown: false,
                mismatch_old: None,
            })),
            tunnel_token: None,
        }
    }
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

/// Public re-export of `pkwh` for the connect-wizard `test_ssh_connect`
/// path so it can share the same RSA hash-alg logic without duplicating it.
pub(crate) fn pkwh_pub(key: PrivateKey) -> Result<PrivateKeyWithHashAlg, String> {
    pkwh(key)
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
        dlog(&format!("ssh.auth: agent probe {label} ({pipe_path})"));
        // Hard 2-second cap on the connect — if Pageant's pipe is alive but
        // its server is wedged, `connect_named_pipe` can block indefinitely.
        let connect_fut =
            russh_keys::agent::client::AgentClient::connect_named_pipe(pipe_path);
        let mut agent = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_fut,
        )
        .await
        {
            Ok(Ok(a)) => {
                dlog(&format!("ssh.auth: agent probe {label} CONNECTED"));
                a
            }
            Ok(Err(e)) => {
                dlog(&format!("ssh.auth: agent probe {label} not reachable: {e}"));
                continue;
            }
            Err(_) => {
                dlog(&format!(
                    "ssh.auth: agent probe {label} TIMED OUT after 2s — skipping"
                ));
                continue;
            }
        };
        let identities = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.request_identities(),
        )
        .await
        {
            Ok(Ok(ids)) => {
                dlog(&format!("ssh.auth: agent {label} offered {} identit(y/ies)", ids.len()));
                ids
            }
            Ok(Err(e)) => {
                dlog(&format!("ssh.auth: agent {label} request_identities: {e}"));
                continue;
            }
            Err(_) => {
                dlog(&format!(
                    "ssh.auth: agent {label} request_identities TIMED OUT after 2s — skipping"
                ));
                continue;
            }
        };
        if identities.is_empty() {
            continue;
        }
        any_agent_seen = true;
        for id in identities {
            dlog(&format!("ssh.auth: agent {label} attempting authenticate_publickey_with"));
            match handle.authenticate_publickey_with(user, id, &mut agent).await {
                Ok(true) => {
                    dlog(&format!("ssh.auth: agent {label} authenticated OK"));
                    return Some(true);
                }
                Ok(false) => {
                    dlog(&format!("ssh.auth: agent {label} key not accepted by server"));
                    continue;
                }
                Err(e) => {
                    dlog(&format!("ssh.auth: agent {label} auth error: {e}"));
                    continue;
                }
            }
        }
    }

    if any_agent_seen {
        dlog("ssh.auth: agent probes done — no agent identity worked");
        Some(false)
    } else {
        dlog("ssh.auth: no agent reachable on any pipe");
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
    dlog(&format!(
        "ssh.auth: begin user={} key_path={:?} key_passphrase={} password={}",
        user,
        key_path,
        if key_passphrase.is_some() { "yes" } else { "no" },
        if password.is_some() { "yes" } else { "no" }
    ));

    // 1) ssh-agent (OpenSSH agent / Pageant via named pipe).
    dlog("ssh.auth: step 1 — try_agent_auth");
    if let Some(true) = try_agent_auth(handle, user).await {
        dlog("ssh.auth: step 1 OK (agent)");
        return Ok(true);
    }

    // 2) Explicit key file (with optional passphrase).
    //
    // SSH-key-load Windows fix: `russh_keys::load_secret_key` opens the file
    // through its own internal helper that, on certain russh-keys versions,
    // funnels the path through Win32 in a way that rejects perfectly valid
    // Windows paths with `os error 123` (ERROR_INVALID_NAME) — even when the
    // exact same path opens fine via `ssh -i`. Reproed by Yossi against
    // C:\Users\…\claude_code_key1 while the sibling key claude_code_key
    // worked. We sidestep the bug by reading the file with std::fs ourselves
    // (which uses CreateFileW correctly) and handing the bytes to russh-keys'
    // in-memory parser, `decode_secret_key`. dlog dumps the path bytes so a
    // future "syntax incorrect" report tells us instantly whether the path
    // contains a hidden char.
    if let Some(p) = key_path {
        dlog(&format!(
            "ssh.auth: step 2 — explicit key file {p:?} bytes={:?} len={}",
            p.as_bytes(),
            p.len()
        ));
        let key_text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                let s = e.to_string();
                dlog(&format!("ssh.auth: read {p} ERR: {s}"));
                return Err(format!("load key {p}: {s}"));
            }
        };
        dlog(&format!(
            "ssh.auth: read {p} OK ({} bytes, head={:?})",
            key_text.len(),
            key_text.lines().next().unwrap_or("")
        ));
        match russh_keys::decode_secret_key(&key_text, key_passphrase) {
            Ok(key) => {
                dlog(&format!(
                    "ssh.auth: key {p} decoded — attempting authenticate_publickey"
                ));
                let pkwh = pkwh(key)?;
                let r = handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?;
                dlog(&format!("ssh.auth: step 2 publickey result = {r}"));
                if r {
                    return Ok(true);
                }
            }
            Err(e) => {
                let s = e.to_string();
                dlog(&format!("ssh.auth: decode_secret_key {p} ERR: {s}"));
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
    dlog(&format!("ssh.auth: step 3 — default key paths under {home}/.ssh/"));
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let p = format!("{}/.ssh/{}", home, name);
        if !Path::new(&p).exists() {
            continue;
        }
        dlog(&format!("ssh.auth: step 3 trying {p}"));
        // Same Windows path workaround as step 2 — read with std::fs first.
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                dlog(&format!("ssh.auth: step 3 read {p} skip: {e}"));
                continue;
            }
        };
        if let Ok(key) = russh_keys::decode_secret_key(&text, None) {
            if let Ok(pkwh) = pkwh(key) {
                let r = handle
                    .authenticate_publickey(user, pkwh)
                    .await
                    .map_err(|e| e.to_string())?;
                dlog(&format!("ssh.auth: step 3 {p} result = {r}"));
                if r {
                    return Ok(true);
                }
            }
        }
    }

    // 4) Password (sent to remote, not key passphrase).
    if let Some(pw) = password {
        dlog("ssh.auth: step 4 — password");
        let r = handle
            .authenticate_password(user, pw)
            .await
            .map_err(|e| e.to_string())?;
        dlog(&format!("ssh.auth: step 4 password result = {r}"));
        if r {
            return Ok(true);
        }
    }

    dlog("ssh.auth: ALL methods exhausted, no auth succeeded");
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
    workspace_id: String,
    host: String,
    user: String,
    port: u16,
    key_path: Option<String>,
    key_passphrase: Option<String>,
    password: Option<String>,
    accept_unknown_host: bool,
    cols: u16,
    rows: u16,
    persistent: bool,
) -> Result<String, String> {
    dlog(&format!(
        "spawn_ssh: entry ws={} pane={} target={}@{}:{}",
        workspace_id, pane_id, user, host, port
    ));
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

    dlog(&format!("spawn_ssh: client::connect to {} starting", target));
    let connect_res = client::connect(config, (host.as_str(), port), sh).await;
    dlog(&format!(
        "spawn_ssh: client::connect to {} returned (ok={})",
        target,
        connect_res.is_ok()
    ));
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

    dlog("spawn_ssh: try_authenticate begin");
    let auth_ok = try_authenticate(
        &mut handle,
        &user,
        key_path.as_deref(),
        key_passphrase.as_deref(),
        password.as_deref(),
    )
    .await?;
    dlog(&format!("spawn_ssh: try_authenticate returned ok={auth_ok}"));
    if !auth_ok {
        return Err("authentication failed (agent, key, and password all failed)".into());
    }

    // Phase 6.2: best-effort bootstrap of the winmux Linux binary on the remote.
    // We never block the user's shell on this — failures are surfaced via pane:status.
    dlog("spawn_ssh: bootstrap starting");
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

    // Phase 8.B: wrap the handle in an Arc before the I/O task takes ownership.
    // russh's Handle isn't Clone, but its methods take &self, so multiple owners
    // of an Arc<Handle> can safely call channel_open_direct_tcpip concurrently
    // (each call is just a message into the underlying session task).
    let handle_arc = Arc::new(handle);
    let handle_for_task = Arc::clone(&handle_arc);
    let handle_for_state = Arc::clone(&handle_arc);
    let workspace_id_for_state = workspace_id.clone();

    let id_for_task = id.clone();
    let pane_for_task = pane_id.clone();
    let app_for_task = app.clone();
    let sessions_for_task = state.sessions.clone();
    let pane_sessions_for_task = state.pane_sessions.clone();
    let forwards_for_task = state.forwards.clone();
    let workspace_for_task = workspace_id.clone();
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
        let _ = handle_for_task
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
        cleanup_session_maps(
            &sessions_for_task,
            &pane_sessions_for_task,
            &pane_for_task,
            &id_for_task,
        );
        // Phase 8.B: if this was the last SSH session for the workspace, tear
        // down all of its port forwards.
        let still_alive = sessions_for_task
            .lock()
            .unwrap()
            .values()
            .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_for_task));
        if !still_alive {
            close_workspace_forwards(&forwards_for_task, &workspace_for_task);
        }
        emit_exit(&app_for_task, &id_for_task, exit_reason);
    });

    let tmux_name = if persistent {
        Some(sanitize_tmux_session_name(&pane_id))
    } else {
        None
    };
    state.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Ssh(SshSession {
            tx,
            handle: handle_for_state,
            workspace_id: workspace_id_for_state.clone(),
            tmux_session: tmux_name.clone(),
        }),
    );

    // Phase 11.A: when the user picked persistent mode, wrap the freshly
    // started shell in `tmux new-session -A -s NAME`. The `-A` flag attaches
    // to an existing session of that name (so a reconnect resumes the same
    // shell with all in-flight processes intact) and creates a fresh one
    // otherwise. We `exec` it so the parent shell process is replaced —
    // killing the SSH channel then doesn't double-prompt for shell exit.
    //
    // We also push the env vars the SSH channel just acquired into tmux's
    // global environment so a re-attach to a long-lived session sees the
    // *current* WINMUX_SOCKET_ADDR/TUNNEL_TOKEN/PANE_ID rather than the
    // stale ones from the original creation. The `2>/dev/null` swallows
    // the harmless "no server running" message when this is the first attach.
    if let Some(name) = &tmux_name {
        let sessions_clone = state.sessions.clone();
        let id_clone = id.clone();
        let name_clone = name.clone();
        let socket_addr = if remote_port != 0 {
            format!("127.0.0.1:{}", remote_port)
        } else {
            String::new()
        };
        let token_clone = token.as_str().to_string();
        let pane_for_exec = pane_id.clone();
        tokio::spawn(async move {
            // Wait a touch longer than schedule_setup_injection (which fires
            // at 500ms) so our exec lands AFTER the env exports + setup_command.
            tokio::time::sleep(std::time::Duration::from_millis(900)).await;
            let mut script = String::new();
            if !socket_addr.is_empty() {
                script.push_str(&format!(
                    "tmux set-environment -g WINMUX_SOCKET_ADDR {} 2>/dev/null; ",
                    shell_quote(&socket_addr)
                ));
                script.push_str(&format!(
                    "tmux set-environment -g WINMUX_TUNNEL_TOKEN {} 2>/dev/null; ",
                    shell_quote(&token_clone)
                ));
                script.push_str(&format!(
                    "tmux set-environment -g WINMUX_PANE_ID {} 2>/dev/null; ",
                    shell_quote(&pane_for_exec)
                ));
            }
            script.push_str(&format!(
                "command -v tmux >/dev/null 2>&1 && exec tmux new-session -A -s {} || echo '[winmux] tmux not installed on remote — falling back to plain shell'\r\n",
                shell_quote(&name_clone)
            ));
            let mut sessions = sessions_clone.lock().unwrap();
            if let Some(Session::Ssh(ssh)) = sessions.get_mut(&id_clone) {
                let _ = ssh.tx.send(SshCmd::Data(script.into_bytes()));
            }
        });
    }
    // Phase 8.B race fix: notify any browser pane in this workspace that a
    // fresh resolve is now possible (SSH handle is live → forwards can open).
    // Browser panes that loaded their iframe with `localhost refused to
    // connect` because SSH wasn't ready yet will pick this up and re-resolve.
    let _ = app.emit(
        "pane:browser:resolve-stale",
        serde_json::json!({ "workspace_id": workspace_id_for_state }),
    );
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

// ─── Phase 8.B: SSH local port forwards ─────────────────────────────────────

// Find an SSH handle for the workspace by walking its connected terminal panes.
// Returns the first one found, or None if no terminal pane in the workspace
// currently has an active SSH session.
fn find_ssh_handle_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Arc<client::Handle<SshClient>>> {
    let sessions = state.sessions.lock().unwrap();
    for s in sessions.values() {
        if let Session::Ssh(ssh) = s {
            if ssh.workspace_id == workspace_id {
                return Some(Arc::clone(&ssh.handle));
            }
        }
    }
    None
}

/// Open (or reuse) a TCP listener on `127.0.0.1:<free_port>` that bridges every
/// inbound connection to the workspace's SSH session via a `direct-tcpip` channel
/// targeted at `localhost:<remote_port>`. Returns the local port the iframe should
/// connect to. Idempotent: if a forward already exists for this (workspace, remote)
/// pair, returns the existing local port.
pub(crate) async fn open_forward(
    state: &AppState,
    workspace_id: &str,
    remote_port: u16,
) -> Result<u16, String> {
    dlog(&format!(
        "open_forward: ws={} remote_port={}",
        workspace_id, remote_port
    ));
    {
        let m = state.forwards.lock().unwrap();
        if let Some(e) = m.get(&(workspace_id.to_string(), remote_port)) {
            dlog(&format!(
                "open_forward: cache hit ws={} remote={} -> local={}",
                workspace_id, remote_port, e.local_port
            ));
            return Ok(e.local_port);
        }
    }
    let handle = find_ssh_handle_for_workspace(state, workspace_id).ok_or_else(|| {
        dlog(&format!(
            "open_forward: NO SSH handle for ws={} — connect a terminal pane first",
            workspace_id
        ));
        "no active SSH session for this workspace — connect a terminal pane first".to_string()
    })?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

    let ws_for_task = workspace_id.to_string();
    let forwards_for_task = state.forwards.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    dlog(&format!("forward[{}:{}]: cancelled", ws_for_task, remote_port));
                    break;
                }
                accept = listener.accept() => {
                    let (mut sock, peer) = match accept {
                        Ok(p) => p,
                        Err(e) => {
                            dlog(&format!("forward[{}:{}]: accept err: {e}", ws_for_task, remote_port));
                            continue;
                        }
                    };
                    let h = Arc::clone(&handle);
                    let ws = ws_for_task.clone();
                    tokio::spawn(async move {
                        let chan = h
                            .channel_open_direct_tcpip(
                                "localhost",
                                remote_port as u32,
                                peer.ip().to_string(),
                                peer.port() as u32,
                            )
                            .await;
                        let chan = match chan {
                            Ok(c) => c,
                            Err(e) => {
                                dlog(&format!(
                                    "forward[{}:{}]: open direct_tcpip: {e}",
                                    ws, remote_port
                                ));
                                return;
                            }
                        };
                        let mut chan_stream = chan.into_stream();
                        if let Err(e) =
                            tokio::io::copy_bidirectional(&mut sock, &mut chan_stream).await
                        {
                            // Connection-reset on close is normal; debug-log only.
                            dlog(&format!("forward[{}:{}]: bridge closed: {e}", ws, remote_port));
                        }
                    });
                }
            }
        }
        // Listener drops here, removing the entry too.
        forwards_for_task
            .lock()
            .unwrap()
            .remove(&(ws_for_task.clone(), remote_port));
    });

    state.forwards.lock().unwrap().insert(
        (workspace_id.to_string(), remote_port),
        ForwardEntry {
            local_port,
            cancel: Some(cancel_tx),
        },
    );
    dlog(&format!(
        "forward[{}:{}]: opened on 127.0.0.1:{}",
        workspace_id, remote_port, local_port
    ));
    Ok(local_port)
}

/// Cancel every forward task whose key has the given workspace_id.
pub(crate) fn close_workspace_forwards(forwards: &ForwardMap, workspace_id: &str) {
    let mut m = forwards.lock().unwrap();
    let keys: Vec<(String, u16)> = m
        .keys()
        .filter(|(w, _)| w == workspace_id)
        .cloned()
        .collect();
    for k in keys {
        if let Some(mut e) = m.remove(&k) {
            if let Some(c) = e.cancel.take() {
                let _ = c.send(());
            }
        }
    }
}

/// Pure URL helper. Returns Some((remote_port, scheme, path_and_query)) if the
/// URL is a localhost / 127.0.0.1 http(s) URL we can forward; None otherwise.
pub(crate) fn parse_localhost_url(url: &str) -> Option<(u16, &'static str, String)> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else {
        return None;
    };
    let (host_port, path) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{}", p)),
        None => (rest, String::new()),
    };
    let host_port = host_port.rsplit_once('@').map(|(_, h)| h).unwrap_or(host_port);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (host_port, if scheme == "https" { 443 } else { 80 }),
    };
    if host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" {
        Some((port, scheme, path))
    } else {
        None
    }
}

/// Pane-aware URL resolver: if the pane is a browser pane in an SSH workspace
/// and `forward_localhost` is on, replace localhost host+port with the forwarded
/// 127.0.0.1:<local> address. Pure pass-through otherwise.
pub(crate) async fn resolve_browser_url(
    state: &AppState,
    workspace_id: &str,
    pane_id: &str,
    url: &str,
) -> Result<String, String> {
    dlog(&format!(
        "resolve_browser_url: input ws={} pane={} url={}",
        workspace_id, pane_id, url
    ));
    // Pull out workspace + pane state under a short lock.
    let (forward_on, is_ssh_ws) = {
        let file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let mut forward_on = false;
        if let Some(layout) = &ws.layout {
            if let Some(pane) = find_pane_in(layout, pane_id) {
                if let LayoutNode::Pane { browser, .. } = pane {
                    if let Some(b) = browser {
                        forward_on = b.forward_localhost;
                    }
                }
            }
        }
        // Workspace counts as SSH if any of its terminal panes uses an SSH connection.
        let mut is_ssh = false;
        if let Some(layout) = &ws.layout {
            collect_pane_connection_kinds(layout, &mut is_ssh);
        }
        (forward_on, is_ssh)
    };
    dlog(&format!(
        "resolve_browser_url: forward_on={} is_ssh_ws={}",
        forward_on, is_ssh_ws
    ));
    if !forward_on || !is_ssh_ws {
        dlog("resolve_browser_url: pass-through (no forward)");
        return Ok(url.to_string());
    }
    let (remote_port, scheme, path) = match parse_localhost_url(url) {
        Some(p) => {
            dlog(&format!(
                "resolve_browser_url: parsed localhost — port={} scheme={} path={:?}",
                p.0, p.1, p.2
            ));
            p
        }
        None => {
            dlog("resolve_browser_url: not a localhost URL — pass-through");
            return Ok(url.to_string());
        }
    };
    let local_port = open_forward(state, workspace_id, remote_port).await?;
    let resolved = format!("{}://127.0.0.1:{}{}", scheme, local_port, path);
    dlog(&format!("resolve_browser_url: -> {}", resolved));
    Ok(resolved)
}

// Phase 8.C: read a pane's persisted BrowserState clone, or None if the pane
// doesn't exist or isn't a browser pane.
pub(crate) fn find_browser_state(state: &AppState, pane_id: &str) -> Option<BrowserState> {
    let file = state.workspaces.lock().unwrap();
    for ws in &file.workspaces {
        if let Some(layout) = &ws.layout {
            if let Some(node) = find_pane_in(layout, pane_id) {
                if let LayoutNode::Pane { browser, .. } = node {
                    return browser.clone();
                }
            }
        }
    }
    None
}

pub(crate) fn next_browser_request_id() -> String {
    let n = BROWSER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("br_{:x}_{:x}", t, n)
}

fn find_pane_in<'a>(node: &'a LayoutNode, target: &str) -> Option<&'a LayoutNode> {
    match node {
        LayoutNode::Pane { pane_id, .. } if pane_id == target => Some(node),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_in(first, target).or_else(|| find_pane_in(second, target))
        }
    }
}

// Set `is_ssh = true` if the layout contains any terminal pane whose connection
// is SSH. (Browser panes do not factor in.)
fn collect_pane_connection_kinds(node: &LayoutNode, is_ssh: &mut bool) {
    match node {
        LayoutNode::Pane { connection, .. } => {
            if matches!(connection, Some(Connection::Ssh { .. })) {
                *is_ssh = true;
            }
        }
        LayoutNode::Split { first, second, .. } => {
            collect_pane_connection_kinds(first, is_ssh);
            collect_pane_connection_kinds(second, is_ssh);
        }
    }
}

// ─── Workspace mutation commands ─────────────────────────────────────────────

#[tauri::command]
fn workspaces_load(state: State<'_, AppState>) -> Result<WorkspacesFile, String> {
    let file = state.workspaces.lock().unwrap().clone();
    dlog(&format!(
        "workspaces_load: returning {} workspaces, active={:?}",
        file.workspaces.len(),
        file.active_workspace_id
    ));
    Ok(file)
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
            pane_kind: PaneKind::Terminal,
            connection: Some(input.connection),
            browser: None,
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

/// Phase 8 fix v3: emergency reset for a workspace whose layout has been
/// corrupted (e.g. by the autosave loop that produced deeply nested splits).
/// Replaces the layout with a single fresh terminal pane using the workspace's
/// existing connection if it had one (terminal panes), else local default.
#[tauri::command]
fn workspace_reset_layout(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        // Pick a connection for the fresh pane:
        // 1. The first terminal pane in the (corrupted) layout, if any.
        // 2. The legacy `connection` field on the workspace.
        // 3. Default Local with no shell override.
        let inferred = ws
            .layout
            .as_ref()
            .and_then(first_terminal_connection)
            .or_else(|| ws.connection.clone())
            .unwrap_or(Connection::Local { shell: None });
        ws.layout = Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(inferred),
            browser: None,
            title: None,
            annotation: None,
        });
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

pub(crate) fn first_terminal_connection_pub(node: &LayoutNode) -> Option<Connection> {
    first_terminal_connection(node)
}

fn first_terminal_connection(node: &LayoutNode) -> Option<Connection> {
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } => {
            if matches!(pane_kind, PaneKind::Terminal) {
                connection.clone()
            } else {
                None
            }
        }
        LayoutNode::Split { first, second, .. } => {
            first_terminal_connection(first).or_else(|| first_terminal_connection(second))
        }
    }
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
    // Phase 8.B: tear down any port forwards for the workspace.
    close_workspace_forwards(&state.forwards, &workspace_id);
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
    // Phase 8.A: kind defaults to Terminal (back-compat). Browser also accepts a
    // starting URL — falls back to about:blank if absent.
    pane_kind: Option<PaneKind>,
    browser_url: Option<String>,
) -> Result<WorkspacesFile, String> {
    let kind = pane_kind.unwrap_or(PaneKind::Terminal);
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                let (new_layout, _) =
                    split_pane_in(layout, &pane_id, direction, kind, browser_url);
                ws.layout = Some(new_layout);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Phase 8.A: browser-pane commands ───────────────────────────────────────

#[tauri::command]
fn pane_browser_navigate(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    url: String,
) -> Result<WorkspacesFile, String> {
    if url.is_empty() {
        return Err("empty url".into());
    }
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                    if !b.url.is_empty() && b.url != url {
                        b.history.push(b.url.clone());
                        if b.history.len() > BROWSER_HISTORY_MAX {
                            let drop = b.history.len() - BROWSER_HISTORY_MAX;
                            b.history.drain(0..drop);
                        }
                    }
                    b.url = url.clone();
                }));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn pane_browser_go_back(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                    if let Some(prev) = b.history.pop() {
                        b.url = prev;
                    }
                }));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 8.B: pure URL resolve. Frontend calls this before setting iframe.src.
/// If the pane has `forward_localhost = true`, the workspace has an active SSH
/// session, and the URL targets `localhost:N` / `127.0.0.1:N`, the response is
/// the rewritten `http://127.0.0.1:<local_port>...` URL. Otherwise pass-through.
#[tauri::command]
async fn pane_browser_resolve_url(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
    url: String,
) -> Result<String, String> {
    resolve_browser_url(&state, &workspace_id, &pane_id, &url).await
}

/// Phase 8.C: frontend reports an iframe `load` event. Records the URL on the
/// pane's `last_loaded_url` (so `browser-wait` can short-circuit when the page
/// is already loaded) and drains every pending wait waiter for this pane.
#[tauri::command]
fn pane_browser_loaded(
    state: State<'_, AppState>,
    app: AppHandle,
    pane_id: String,
    url: String,
) -> Result<(), String> {
    dlog(&format!(
        "pane_browser_loaded: pane={} url={}",
        pane_id, url
    ));
    // Stamp last_loaded_url on the pane's BrowserState. Dedupe: only persist
    // if the value actually changed — iframe onload can fire repeatedly
    // (focus changes, redirects) and persisting on every fire would burn
    // disk + spam debug.log without benefit.
    let mut changed = false;
    {
        let mut file = state.workspaces.lock().unwrap();
        for ws in &mut file.workspaces {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                    if b.last_loaded_url.as_deref() != Some(url.as_str()) {
                        b.last_loaded_url = Some(url.clone());
                        changed = true;
                    }
                }));
            }
        }
    }
    if changed {
        let _ = persist(&state);
    }
    let _ = app;
    let waiters = state
        .browser_load_waiters
        .lock()
        .unwrap()
        .remove(&pane_id)
        .unwrap_or_default();
    for tx in waiters {
        let _ = tx.send(url.clone());
    }
    Ok(())
}

/// Phase 8.C: frontend delivers a response to a pending browser request
/// (screenshot, eval). Called by BrowserPane.tsx after handling a `browser:request`
/// event. The backend resolves the matching oneshot.
#[tauri::command]
fn pane_browser_response(
    state: State<'_, AppState>,
    request_id: String,
    ok: Option<serde_json::Value>,
    err: Option<String>,
) -> Result<(), String> {
    if let Some(tx) = state.browser_pending.lock().unwrap().remove(&request_id) {
        let payload: Result<serde_json::Value, String> = match err {
            Some(e) => Err(e),
            None => Ok(ok.unwrap_or(serde_json::Value::Null)),
        };
        let _ = tx.send(payload);
    }
    Ok(())
}

// ─── Phase 8.F.1: iframe-bridge round-trip ─────────────────────────────────

fn next_iframe_request_id() -> String {
    let n = IFRAME_REQ_COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ifr_{:x}_{:x}", t, n)
}

/// Send a command into the iframe of `pane_id`, wait for the iframe's
/// postMessage response to come back through the parent bridge, return it.
///
/// Wire path: this fn → eval'd JS in main webview → `iframe.contentWindow.postMessage`
/// → bridge JS in iframe → `runCommand(cmd, args)` → `window.parent.postMessage`
/// → bridge JS in parent → `pane_browser_iframe_response` Tauri cmd → resolves
/// the oneshot in `iframe_pending` keyed by `request_id`.
pub(crate) async fn iframe_cmd_inner(
    state: &AppState,
    app: &AppHandle,
    pane_id: &str,
    cmd: &str,
    args: serde_json::Value,
    timeout_ms: u64,
) -> Result<serde_json::Value, String> {
    let request_id = next_iframe_request_id();
    dlog(&format!(
        "iframe_cmd: pane={} cmd={} request_id={} timeout_ms={}",
        pane_id, cmd, request_id, timeout_ms
    ));

    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .iframe_pending
        .lock()
        .unwrap()
        .insert(request_id.clone(), tx);

    let message = serde_json::to_string(&serde_json::json!({
        "winmux": true,
        "role": "command",
        "request_id": request_id,
        "cmd": cmd,
        "args": args,
    }))
    .map_err(|e| format!("serialize message: {e}"))?;

    // Escape pane_id for inclusion in a JS string literal.
    let pane_id_js = pane_id.replace('\\', "\\\\").replace('\'', "\\'");
    let script = format!(
        "(function(){{\
           const ifr = document.querySelector('iframe[data-pane-id=\"{pane}\"]');\
           if (!ifr || !ifr.contentWindow) {{ console.error('winmux: no iframe pane', '{pane}'); return; }}\
           ifr.contentWindow.postMessage({msg}, '*');\
         }})();",
        pane = pane_id_js,
        msg = message
    );

    let win = app
        .get_webview_window("main")
        .ok_or("no main window")?;
    win.eval(&script).map_err(|e| format!("eval: {e}"))?;

    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await;
    match result {
        Ok(Ok(Ok(v))) => Ok(v),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err("response channel closed".into()),
        Err(_) => {
            state.iframe_pending.lock().unwrap().remove(&request_id);
            Err(format!("iframe cmd timeout after {timeout_ms}ms"))
        }
    }
}

#[tauri::command]
async fn pane_browser_iframe_cmd(
    state: State<'_, AppState>,
    app: AppHandle,
    pane_id: String,
    cmd: String,
    args: serde_json::Value,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    iframe_cmd_inner(
        &state,
        &app,
        &pane_id,
        &cmd,
        args,
        timeout_ms.unwrap_or(5_000),
    )
    .await
}

/// Frontend bridge calls this with the iframe's postMessage response. Resolves
/// the oneshot waiting in `iframe_pending`.
#[tauri::command]
fn pane_browser_iframe_response(
    state: State<'_, AppState>,
    request_id: String,
    ok: bool,
    result: Option<serde_json::Value>,
    error: Option<String>,
) -> Result<(), String> {
    let tx = state.iframe_pending.lock().unwrap().remove(&request_id);
    if let Some(tx) = tx {
        let payload = if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(error.unwrap_or_else(|| "iframe error".to_string()))
        };
        let _ = tx.send(payload);
    }
    Ok(())
}

/// Phase 8.E: frontend console.error/warn capture. Pushes one entry into the
/// ring buffer; the CLI surfaces them via `winmux dev console-tail`.
#[tauri::command]
fn dev_console_log(
    state: State<'_, AppState>,
    level: String,
    message: String,
    ts: i64,
) -> Result<(), String> {
    dev::push_console(
        &state.console_buffer,
        dev::ConsoleEntry {
            level,
            message,
            ts,
        },
    );
    Ok(())
}

/// Phase 8.B: per-pane toggle for the localhost-forwarding behavior. Sticky.
#[tauri::command]
fn pane_browser_set_forward(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    forward: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                    b.forward_localhost = forward;
                }));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

#[tauri::command]
fn pane_browser_go_home(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                ws.layout = Some(update_browser_pane(layout, &pane_id, &mut |b| {
                    if let Some(home) = b.home_url.clone() {
                        if !b.url.is_empty() && b.url != home {
                            b.history.push(b.url.clone());
                            if b.history.len() > BROWSER_HISTORY_MAX {
                                let drop = b.history.len() - BROWSER_HISTORY_MAX;
                                b.history.drain(0..drop);
                            }
                        }
                        b.url = home;
                    }
                }));
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
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
    // Phase 11.A: when true the SSH shell is wrapped in `tmux new-session -A`
    // so reconnects resume the same shell. Ignored for local panes for now —
    // can be added later via WSL/conpty + tmux on linux.
    persistent: Option<bool>,
    // Phase 12.B Smart Connect: when set, after the shell is up we inject a
    // mode-specific command. `mode` is one of: "default" (current behavior),
    // "tmux" (alias for persistent=true), "plain" (no tmux even if workspace
    // says persistent), "cmd" (run cmd in cwd), "claude" (launch claude in
    // cwd with claude_args).
    mode: Option<String>,
    cwd_override: Option<String>,
    cmd: Option<String>,
    claude_args: Option<String>,
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
        let conn = find_pane_connection(layout, &pane_id).ok_or_else(|| {
            if pane_id_exists_in(layout, &pane_id) {
                format!("pane {pane_id} is not a terminal pane")
            } else {
                format!("no pane {pane_id}")
            }
        })?;
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
            // Phase 12.B: derive effective persistence from mode if given.
            // mode="tmux" → persistent regardless of caller; mode="plain"
            // → forced plain; otherwise honor `persistent` flag.
            let effective_persistent = match mode.as_deref() {
                Some("tmux") => true,
                Some("plain") => false,
                _ => persistent.unwrap_or(false),
            };
            spawn_ssh(
                &state,
                pane_id.clone(),
                &app,
                workspace_id.clone(),
                host,
                user,
                port,
                key_path,
                key_passphrase,
                password,
                accept_unknown_host.unwrap_or(false),
                cols,
                rows,
                effective_persistent,
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

    // Phase 12.B Smart Connect: when mode is "cmd" or "claude", inject the
    // command after a 1.1s delay (after env exports + setup_command + tmux
    // wrap have all settled). cwd_override changes directory first. We use
    // `exec` so the launched process replaces the shell — the user gets
    // back to a clean prompt only when the command exits.
    let smart_mode = mode.clone();
    if matches!(smart_mode.as_deref(), Some("cmd") | Some("claude")) {
        let sessions_clone = state.sessions.clone();
        let session_id_clone = session_id.clone();
        let mode_str = smart_mode.unwrap_or_default();
        let cwd_str = cwd_override.clone();
        let cmd_str = cmd.clone();
        let claude_args_str = claude_args.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            let mut script = String::new();
            if let Some(cwd) = cwd_str.filter(|s| !s.is_empty()) {
                script.push_str(&format!("cd {} && ", shell_quote(&cwd)));
            }
            match mode_str.as_str() {
                "cmd" => {
                    if let Some(c) = cmd_str.filter(|s| !s.trim().is_empty()) {
                        script.push_str(&format!("exec {}\r\n", c));
                    }
                }
                "claude" => {
                    let args = claude_args_str.unwrap_or_default();
                    let trimmed = args.trim();
                    if trimmed.is_empty() {
                        script.push_str("exec claude\r\n");
                    } else {
                        script.push_str(&format!("exec claude {}\r\n", trimmed));
                    }
                }
                _ => {}
            }
            if !script.is_empty() {
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(s) = sessions.get_mut(&session_id_clone) {
                    match s {
                        Session::Local(l) => {
                            use std::io::Write as _;
                            let _ = l.writer.write_all(script.as_bytes());
                            let _ = l.writer.flush();
                        }
                        Session::Ssh(ssh) => {
                            let _ = ssh.tx.send(SshCmd::Data(script.into_bytes()));
                        }
                    }
                }
            }
        });
    }

    Ok(session_id)
}

/// Phase 12.B: Claude Code session metadata returned by
/// pane_list_claude_sessions for the session-picker modal.
#[derive(Clone, Serialize)]
pub(crate) struct ClaudeSessionInfo {
    pub session_id: String,
    pub project_path: String,
    pub jsonl_path: String,
    pub mtime_unix: i64,
    /// First user message preview (best-effort; first ~80 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_user: Option<String>,
    /// Last assistant message preview (best-effort).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant: Option<String>,
}

/// Phase 12.B: list recent Claude Code sessions on the workspace's host.
/// For SSH workspaces with a live session, reuses the existing SSH handle
/// to open a fresh exec channel (no extra auth round-trip). For local
/// workspaces, reads `~/.claude/projects/*/sessions/*.jsonl` directly.
/// Best-effort: if the path doesn't exist or jq isn't installed we still
/// return what we can (path + mtime, no previews).
#[tauri::command]
async fn pane_list_claude_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
    limit: Option<usize>,
) -> Result<Vec<ClaudeSessionInfo>, String> {
    let limit = limit.unwrap_or(30).min(200);
    // Locate any live SSH handle for this workspace. The shell command runs
    // on the remote where Claude Code is actually installed.
    let handle_opt = {
        let sessions = state.sessions.lock().unwrap();
        sessions
            .iter()
            .find_map(|(_sid, sess)| match sess {
                Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
                _ => None,
            })
    };

    let script = format!(
        "find \"$HOME/.claude/projects\" -maxdepth 4 -name '*.jsonl' \
         -printf '%T@\\t%p\\n' 2>/dev/null | sort -rn | head -{} | \
         while IFS=$'\\t' read -r mt path; do \
           first_user=$(head -100 \"$path\" 2>/dev/null | \
             grep -m1 -E '\"role\"\\s*:\\s*\"user\"' | head -c 600); \
           last_asst=$(tail -200 \"$path\" 2>/dev/null | \
             grep -E '\"role\"\\s*:\\s*\"assistant\"' | tail -1 | head -c 600); \
           printf '%s\\t%s\\t%s\\t%s\\n' \"$mt\" \"$path\" \"$first_user\" \"$last_asst\"; \
         done",
        limit
    );

    let raw = if let Some(handle) = handle_opt {
        // Run via SSH exec.
        let mut ch = handle
            .channel_open_session()
            .await
            .map_err(|e| format!("channel_open: {e}"))?;
        ch.exec(true, script.as_bytes())
            .await
            .map_err(|e| format!("exec: {e}"))?;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while let Some(msg) = ch.wait().await {
                match msg {
                    russh::ChannelMsg::Data { ref data } => out.extend_from_slice(data),
                    russh::ChannelMsg::ExtendedData { .. } => {}
                    russh::ChannelMsg::Eof | russh::ChannelMsg::Close | russh::ChannelMsg::ExitStatus { .. } => break,
                    _ => {}
                }
            }
        })
        .await;
        let _ = ch.close().await;
        String::from_utf8_lossy(&out).to_string()
    } else {
        // No SSH session live → run locally on Windows. Translate to a small
        // walk of %USERPROFILE%\.claude\projects\*\*.jsonl. We don't try to
        // mirror the full bash pipeline — just enumerate, sort by mtime,
        // return path + mtime; previews are skipped.
        return list_claude_sessions_local(limit);
    };

    let mut out = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let mtime = parts[0]
            .split('.')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let path = parts[1].to_string();
        let last_user = parts.get(2).map(|s| extract_text_field(s));
        let last_asst = parts.get(3).map(|s| extract_text_field(s));
        let session_id = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let project_path = std::path::Path::new(&path)
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            // Claude Code encodes paths with `-` for `/`. We surface the raw
            // dirname; a future polish can decode it back to a real path.
            .unwrap_or("?")
            .to_string();
        out.push(ClaudeSessionInfo {
            session_id,
            project_path,
            jsonl_path: path,
            mtime_unix: mtime,
            last_user: last_user.filter(|s| !s.is_empty()),
            last_assistant: last_asst.filter(|s| !s.is_empty()),
        });
    }
    Ok(out)
}

fn list_claude_sessions_local(limit: usize) -> Result<Vec<ClaudeSessionInfo>, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(std::path::PathBuf, i64)> = Vec::new();
    if let Ok(it) = std::fs::read_dir(&root) {
        for proj in it.flatten() {
            if let Ok(it2) = std::fs::read_dir(proj.path()) {
                for f in it2.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        let mtime = f
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| {
                                t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
                            })
                            .unwrap_or(0);
                        entries.push((p, mtime));
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    entries.truncate(limit);
    let mut out = Vec::new();
    for (p, mtime) in entries {
        let session_id = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let project_path = p
            .parent()
            .and_then(|q| q.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        out.push(ClaudeSessionInfo {
            session_id,
            project_path,
            jsonl_path: p.to_string_lossy().to_string(),
            mtime_unix: mtime,
            last_user: None,
            last_assistant: None,
        });
    }
    Ok(out)
}

/// Best-effort extractor: pulls the first occurrence of `"text":"…"` (or
/// `"content":"…"` as a fallback) out of a fragment of a JSONL line, with
/// the JSON-escape sequences decoded. Sufficient for the preview column;
/// not a full JSON parser. Returns the trimmed first ~80 chars.
fn extract_text_field(fragment: &str) -> String {
    fn extract_one(s: &str, key: &str) -> Option<String> {
        let needle = format!("\"{}\":\"", key);
        let idx = s.find(&needle)?;
        let mut chars = s[idx + needle.len()..].chars().peekable();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => out.push('"'),
                    Some('n') => out.push(' '),
                    Some('t') => out.push(' '),
                    Some('r') => {}
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some(other) => out.push(other),
                    None => break,
                }
            } else if c == '"' {
                break;
            } else {
                out.push(c);
            }
            if out.len() > 600 {
                break;
            }
        }
        Some(out)
    }
    let extracted = extract_one(fragment, "text")
        .or_else(|| extract_one(fragment, "content"))
        .unwrap_or_default();
    let trimmed = extracted.trim();
    if trimmed.chars().count() <= 80 {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(80).collect();
        out.push('…');
        out
    }
}

/// Phase 11.A: introspection — is this pane currently bound to a tmux
/// persistent session? Used by the frontend to render the `T` badge and
/// to decide whether the disconnect dropdown should expose "Kill session".
#[tauri::command]
fn pane_persistence_get(
    state: State<'_, AppState>,
    pane_id: String,
) -> Option<String> {
    let sessions_map = state.pane_sessions.lock().unwrap();
    let sid = sessions_map.get(&pane_id)?.clone();
    drop(sessions_map);
    let sessions = state.sessions.lock().unwrap();
    if let Some(Session::Ssh(s)) = sessions.get(&sid) {
        return s.tmux_session.clone();
    }
    None
}

/// Phase 11.A: list every (pane_id → tmux_session_name) currently active.
/// Frontend uses this on workspaces:changed / pty:exit to refresh badges
/// without having to query each pane individually.
#[tauri::command]
fn pane_persistence_list(
    state: State<'_, AppState>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let pane_sessions = state.pane_sessions.lock().unwrap().clone();
    let sessions = state.sessions.lock().unwrap();
    for (pane, sid) in pane_sessions {
        if let Some(Session::Ssh(s)) = sessions.get(&sid) {
            if let Some(name) = &s.tmux_session {
                out.insert(pane, name.clone());
            }
        }
    }
    out
}

/// Phase 11.A: hard-kill the tmux session bound to this pane. Opens a fresh
/// exec channel on the existing SSH handle, runs `tmux kill-session -t NAME`,
/// then closes the original shell channel. Falls through to a plain
/// disconnect for non-tmux panes so `winmux pane-disconnect --kill` is
/// always meaningful regardless of which mode the pane was started in.
#[tauri::command]
async fn pane_kill_session(
    state: State<'_, AppState>,
    pane_id: String,
) -> Result<(), String> {
    let sid_opt = state.pane_sessions.lock().unwrap().get(&pane_id).cloned();
    let Some(sid) = sid_opt else {
        return Ok(());
    };
    // Snapshot the SSH handle + tmux name without holding the lock across the
    // .await — russh's Handle is shared as Arc<> so this is cheap.
    let (handle_arc, tmux_name) = {
        let sessions = state.sessions.lock().unwrap();
        match sessions.get(&sid) {
            Some(Session::Ssh(s)) => (Some(s.handle.clone()), s.tmux_session.clone()),
            _ => (None, None),
        }
    };
    if let (Some(handle), Some(name)) = (handle_arc, tmux_name) {
        let cmd = format!("tmux kill-session -t {} 2>&1 || true", shell_quote(&name));
        match handle.channel_open_session().await {
            Ok(mut ch) => {
                if let Err(e) = ch.exec(true, cmd.as_bytes()).await {
                    dlog(&format!("pane_kill_session: exec failed: {e}"));
                }
                // Drain the channel briefly so the server completes the exec.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(800),
                    async {
                        while let Some(msg) = ch.wait().await {
                            if matches!(msg, ChannelMsg::ExitStatus { .. } | ChannelMsg::Eof | ChannelMsg::Close) {
                                break;
                            }
                        }
                    },
                )
                .await;
                let _ = ch.close().await;
            }
            Err(e) => {
                dlog(&format!("pane_kill_session: channel_open failed: {e}"));
            }
        }
    }
    // Now close the shell + remove session bookkeeping. This re-uses the
    // existing pane_disconnect logic by removing from pane_sessions and
    // killing the underlying session.
    let sid = state.pane_sessions.lock().unwrap().remove(&pane_id);
    if let Some(sid) = sid {
        if let Some(mut s) = state.sessions.lock().unwrap().remove(&sid) {
            kill_session_inner(&mut s);
        }
    }
    Ok(())
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
            // Phase 8.E hotfix: log the exact config dir up front so we can
            // tell whether the binary is resolving the right path. Honors
            // `WINMUX_CONFIG_DIR` env var override if set.
            let cfg_dir = config_dir().ok();
            dlog(&format!(
                "setup: config_dir = {:?} (override env WINMUX_CONFIG_DIR = {:?})",
                cfg_dir,
                std::env::var("WINMUX_CONFIG_DIR").ok()
            ));
            tracing::info!("winmux config_dir: {:?}", cfg_dir);

            // Phase 8.F.1: create the main window programmatically so we can
            // inject the iframe-bridge initialization script into every frame
            // (including cross-origin iframes — that's the only path Tauri 2
            // exposes for setting WebView2's AddScriptToExecuteOnDocumentCreated).
            // tauri.conf.json's `windows: []` skips the default window so this
            // is the only one created. Settings here mirror the previous conf:
            // title "winmux", inner size 1100x700.
            const BRIDGE_JS: &str = include_str!("winmux_bridge.js");
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("winmux")
            .inner_size(1100.0, 700.0)
            .initialization_script_for_all_frames(BRIDGE_JS)
            .build()
            .map_err(|e| Box::<dyn std::error::Error>::from(format!("main window: {e}")))?;
            dlog("setup: main webview created with iframe bridge");
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
            // Phase 9.A: load settings (or write defaults on first run).
            match settings::load_from_disk() {
                Ok(s) => {
                    dlog(&format!("setup: settings loaded (theme.preset={})", s.theme.preset));
                    *state.settings.lock().unwrap() = s;
                }
                Err(e) => {
                    dlog(&format!("setup: settings load failed: {e} (using defaults)"));
                }
            }
            // Phase 9.B: spawn the update checker if enabled. Fully best-effort —
            // never blocks startup; failures (offline, manifest missing, repo
            // private) just log to debug.log and emit nothing.
            {
                let s = state.settings.lock().unwrap().clone();
                if s.updates.check_on_startup {
                    let app_handle = app.handle().clone();
                    let state_clone: AppState = (*state).clone();
                    tauri::async_runtime::spawn(async move {
                        // Small delay so the splash + initial render finish first.
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        let _ = updater::check(&state_clone, &app_handle).await;
                    });
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
            pane_kill_session,
            pane_persistence_get,
            pane_persistence_list,
            pane_list_claude_sessions,
            pane_set_title,
            pane_set_annotation,
            pane_browser_navigate,
            pane_browser_go_back,
            pane_browser_go_home,
            pane_browser_resolve_url,
            pane_browser_set_forward,
            pane_browser_response,
            pane_browser_loaded,
            pane_browser_iframe_cmd,
            pane_browser_iframe_response,
            workspace_reset_layout,
            dev_console_log,
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
            settings::settings_load,
            settings::settings_save,
            settings::settings_get_presets,
            settings::settings_apply_preset,
            settings::settings_reset,
            settings::list_system_fonts,
            updater::check_for_updates_now,
            connect_wizard::parse_ssh_config,
            connect_wizard::list_ssh_keys,
            connect_wizard::check_key_permissions,
            connect_wizard::fix_key_permissions,
            connect_wizard::test_ssh_connect,
            provisioning::provisioning_inspect,
            provisioning::provisioning_start,
            provisioning::provisioning_profiles_list,
            provisioning::provisioning_profile_save,
            provisioning::provisioning_profile_delete,
            provisioning::provisioning_step_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
