// Phase 24.D: claude_chat module deleted with the ClaudeChat pane.
mod bidi_filter;
// Phase 53 (rebased): browser_pane.rs renamed to workspace_browser.rs;
// per-pane commands swapped for workspace-keyed commands.
mod workspace_browser;
mod claude_log;
mod claude_summary;
mod connect_wizard;
mod dev;
mod diff_pane;
mod file_manager;
mod local_wizard;
mod notes;
mod osc_notify;
mod provisioning;
mod remote_bootstrap;
mod rpc_server;
mod settings;
mod stt;
mod updater;
// Phase 51.C: `mod tunnel` moved to its own crate winmux-tunnel.
// Existing crate::tunnel::* callsites still resolve via this alias.
use winmux_tunnel as tunnel;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use russh::client;
use russh::ChannelMsg;
// Phase 51.H: russh-keys imports removed (now used only inside winmux-ssh).

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
static PANE_COUNTER: AtomicU64 = AtomicU64::new(0);
static SPLIT_COUNTER: AtomicU64 = AtomicU64::new(0);

// Phase 51.B3: Session/LocalSession/SshSession/SshCmd + SessionMap
// moved to winmux-core. Re-exported below so existing crate::Session,
// crate::SshSession, crate::SshCmd references resolve unchanged.
pub(crate) use winmux_core::{LocalSession, Session, SessionMap, SshCmd, SshSession};
// PaneSessionMap moved to winmux-core (51.B4).
type WorkspacesState = Arc<Mutex<WorkspacesFile>>;

// Phase 51.B3: ForwardEntry + ForwardMap moved to winmux-core.
// Phase 51.B4: PaneSessionMap + CoreState live in winmux-core too.
pub(crate) use winmux_core::{CoreState, ForwardEntry, ForwardMap, PaneSessionMap};


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

#[derive(Clone, Serialize, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeedItemState {
    Pending,
    Allowed,
    Denied,
    Timedout,
    Passive,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/")]
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
    // serde_json::Value has no fixed shape; surface it as `unknown` on
    // the TS side (caller narrows) rather than ts-rs's default `any`.
    #[ts(type = "unknown")]
    pub(crate) payload: serde_json::Value,
    pub(crate) state: FeedItemState,
    #[ts(type = "number")]
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

/// Phase 51.B4: the 9 russh/session/forwards/tunnel runtime fields
/// previously inline here moved into `winmux_core::CoreState`. The
/// outer AppState now wraps it and adds the tauri/notes/settings/
/// dev/feed/browser/claude/console/iframe fields that the
/// application shell needs. Callsites access russh state through
/// `state.core.<field>` (e.g. `state.core.sessions.lock()`).
#[derive(Default, Clone)]
pub(crate) struct AppState {
    pub(crate) core: CoreState,
    pub(crate) workspaces: WorkspacesState,
    pub(crate) load_state: Arc<Mutex<Option<LoadState>>>,
    pub(crate) notifications: Arc<Mutex<Vec<NotificationItem>>>,
    pub(crate) pane_status: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) feed: Arc<Mutex<FeedStore>>,
    pub(crate) notes: Arc<Mutex<notes::NotesFile>>,
    // Phase 9.A: persistent app settings (theme, fonts, terminal, hooks, etc.)
    pub(crate) settings: Arc<Mutex<settings::Settings>>,
    // Phase 12.C: small history of recently-used cwds for local PTY workspaces.
    pub(crate) recent_paths: Arc<Mutex<local_wizard::RecentPathsFile>>,
    // Phase 8.E: ring buffer of frontend console.error/warn captures.
    pub(crate) console_buffer: dev::ConsoleBuffer,
    /// Phase 22.B-fix: cached absolute path to the `claude` binary,
    /// keyed by `<workspace_id>:<scope>` where scope is "ssh" or
    /// "local". Detection runs on first chat-send and the result
    /// sticks for the rest of the session — saves a roundtrip per
    /// message and survives the non-interactive-shell PATH gotcha
    /// (SSH execs do NOT source ~/.bashrc, so a `claude` only on
    /// the user's interactive PATH is otherwise invisible).
    pub(crate) claude_paths: Arc<Mutex<HashMap<String, String>>>,
    /// Phase 52 (BiDi 33B): per-pane PTY-stream bidi filter state. The
    /// filter type lives in `app` (not winmux-core) since it's a
    /// feature concern, not core russh/sessions. Lazy-created on
    /// first chunk per pane; toggled via `pane_set_smart_bidi`.
    pub(crate) bidi_filters: bidi_filter::BidiFilterMap,
    /// Phase 53 (rebased): per-workspace child Webview for the
    /// floating Browser window. At most one Webview per workspace
    /// keyed by `workspace_id`. Lives only at runtime — never
    /// persisted to workspaces.json. `workspace_delete` also calls
    /// `workspace_browser::cleanup_workspace_sessions` to remove the
    /// matching `browser-sessions/<workspace_id>/` directory.
    pub(crate) workspace_browsers: workspace_browser::WorkspaceBrowserMap,
    /// Phase 62.A (item D): serializes native Browser Webview creation.
    /// WebView2's `add_child` intermittently returns 0x8007139F
    /// (ERROR_INVALID_STATE) when two creations race, or when a
    /// just-closed webview hasn't fully released its WebView2
    /// environment. Held across the (retrying) slow path in
    /// `workspace_browser_show` so at most one creation runs at a time.
    pub(crate) browser_create_lock: Arc<tokio::sync::Mutex<()>>,
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
//
// Phase 51.A: the data types previously defined inline here moved to
// the `winmux-types` crate (app/src-tauri/crates/winmux-types/) so
// future split crates (ssh, pty, feed, rpc) can reference them without
// pulling in tauri. Re-exported below so all existing
// `crate::Connection` / `crate::Workspace` / etc. paths continue to
// resolve unchanged. ts-rs bindings are generated by the sub-crate's
// own derive — `cargo test` still regenerates `app/src/bindings/*.ts`
// since the export_to path resolves to the same on-disk location.
pub(crate) use winmux_types::{
    BrowserState, Connection, DiffSource, EnvVar, LayoutNode, PaneKind, SplitDirection, Workspace,
};

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

// Phase 51.B1: config_dir + dlog + shell_quote + pure layout walkers
// moved to winmux-core. Re-exported below so every existing
// `crate::dlog` / `crate::shell_quote` / `crate::collect_panes` /
// `crate::first_terminal_connection_pub` / `crate::backfill_terminal_connections`
// callsite resolves unchanged.
pub(crate) use winmux_core::{
    backfill_terminal_connections, collect_panes, collect_panes_with_kind, config_dir,
    config_dir_pub, dlog, first_terminal_connection, first_terminal_connection_pub, shell_quote,
};

/// Phase 38: absolute path to the debug log, for the Settings → Logs
/// UI ("Open folder" / "Copy path"). Single source of truth — matches
/// exactly what `dlog` writes to.
#[tauri::command]
fn log_dir_path() -> Result<String, String> {
    Ok(config_dir()?.join("debug.log").to_string_lossy().to_string())
}

/// Phase 39: last `n` lines of debug.log for the Logs tab viewer. Only
/// the tail end of the file is read (seek from EOF, ~256 KB window) so
/// a multi-MB log doesn't get slurped whole on every 5s refresh.
#[tauri::command]
fn read_log_tail(n: usize) -> Result<String, String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = config_dir()?.join("debug.log");
    if !path.exists() {
        return Ok(String::new());
    }
    let mut f = std::fs::File::open(&path).map_err(|e| format!("open log: {e}"))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    // Read at most the last 256 KB — comfortably more than 200 lines.
    const WINDOW: u64 = 256 * 1024;
    let start = len.saturating_sub(WINDOW);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("read log: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    // If we started mid-file, drop the first (likely partial) line.
    let text = if start > 0 {
        text.splitn(2, '\n').nth(1).unwrap_or("")
    } else {
        &text
    };
    let lines: Vec<&str> = text.lines().collect();
    let tail = if lines.len() > n {
        &lines[lines.len() - n..]
    } else {
        &lines[..]
    };
    Ok(tail.join("\n"))
}

fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("workspaces.json"))
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
            // Legacy: workspace existed without a layout. Build a
            // single Terminal pane and seed its connection from the
            // workspace's legacy `connection` field. Keep the same
            // value on the workspace too (Phase 23.D: workspace.connection
            // is now canonical, not consumed).
            let conn = ws
                .connection
                .clone()
                .unwrap_or(Connection::Local { shell: None });
            ws.connection = Some(conn.clone());
            ws.layout = Some(LayoutNode::Pane {
                pane_id: new_pane_id(),
                pane_kind: PaneKind::Terminal,
                connection: Some(conn),
                browser: None,
                title: None,
                annotation: None,
                color: None,
                emoji: None,
                help_topic: None,
                diff_source: None,
                smart_bidi: None,
            });
            migrated = true;
        }
        // Phase 23.D: ensure every workspace has a canonical
        // `connection` field. Old files where the connection lived
        // only on the first Terminal pane get back-filled here. This
        // is what lets pane_connect / split / the frontend dropdown
        // fall back to the workspace's intended connection when a
        // pane doesn't have one of its own (FileManager / Browser /
        // ClaudeChat panes, or a fresh pane added later).
        if ws.connection.is_none() {
            if let Some(layout) = ws.layout.as_ref() {
                if let Some(conn) = first_terminal_connection(layout) {
                    ws.connection = Some(conn);
                    migrated = true;
                }
            }
        }
        // Phase 24.D: rescue Terminal panes that have no connection
        // — most commonly those are former ClaudeChat (Phase 22) or
        // ClaudeLog (Phase 24.B) panes whose PaneKind got aliased
        // back to Terminal at deserialize time but whose connection
        // field was always None. Backfill from ws.connection (which
        // by now is guaranteed to be Some via the block just above)
        // so they're usable instead of dead.
        if let Some(layout) = ws.layout.take() {
            let (new_layout, changed) =
                backfill_terminal_connections(layout, &ws.connection);
            ws.layout = Some(new_layout);
            if changed {
                migrated = true;
                dlog(&format!(
                    "load_from_disk: ws={} backfilled Terminal pane connections \
                     (claudechat/claudelog → Terminal migration)",
                    ws.id
                ));
            }
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
/// Phase 39.B: flip every workspace whose `auto_port_forward` is true
/// to false. Returns how many were changed (0 on a second run — the
/// migration is idempotent at the data level too, independent of the
/// settings flag).
pub(crate) fn disable_all_auto_port_forward(file: &mut WorkspacesFile) -> usize {
    let mut n = 0;
    for ws in file.workspaces.iter_mut() {
        if ws.auto_port_forward {
            ws.auto_port_forward = false;
            n += 1;
        }
    }
    n
}

/// Phase 53 (rebased): rewrite every PaneKind::Browser /
/// PaneKind::FileManager pane in the file to PaneKind::Terminal. The
/// per-pane Browser / FileManager surface was replaced by workspace-
/// level singleton floating windows; the leftover panes would render
/// as broken under the new layout, so we collapse them to Terminal
/// on first load post-upgrade and reset their `connection` (the
/// inheritance chain in `workspace_split` rehydrates a sensible
/// fallback the next time the pane is touched).
///
/// Returns the count of panes rewritten. Idempotent — a second call
/// finds none to flip.
#[allow(deprecated)]
pub(crate) fn rewrite_browser_filemanager_panes_to_terminal(
    file: &mut WorkspacesFile,
) -> usize {
    fn walk(node: &mut LayoutNode, count: &mut usize) {
        match node {
            LayoutNode::Pane { pane_kind, .. } => {
                if matches!(pane_kind, PaneKind::Browser | PaneKind::FileManager) {
                    *pane_kind = PaneKind::Terminal;
                    *count += 1;
                }
            }
            LayoutNode::Split { first, second, .. } => {
                walk(first, count);
                walk(second, count);
            }
        }
    }
    let mut n = 0;
    for ws in file.workspaces.iter_mut() {
        if let Some(layout) = ws.layout.as_mut() {
            walk(layout, &mut n);
        }
    }
    n
}

pub(crate) fn persist(state: &AppState) -> Result<(), String> {
    // Phase 59.E: caller-location trace demoted dlog → tracing::debug.
    // It fires on EVERY workspace mutation (ratio commits, title
    // edits, splits…) and is engineer diagnostics, not user-facing —
    // Rule 9 audience split. The REFUSING branches below stay on
    // dlog: those are the lines a user needs to see when their
    // workspaces.json stopped saving.
    let caller = std::panic::Location::caller();
    tracing::debug!("persist: called from {}:{}", caller.file(), caller.line());
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

/// Phase 23.I: look up a pane's user-set title in a layout tree.
/// Used by `pane_connect` to derive a tmux session name from the
/// title (pane title IS the tmux session name).
pub(crate) fn find_pane_title(node: &LayoutNode, target: &str) -> Option<String> {
    match node {
        LayoutNode::Pane { pane_id, title, .. } if pane_id == target => title.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            find_pane_title(first, target).or_else(|| find_pane_title(second, target))
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

// Phase 24.D: update_chat_pane (Phase 22) and update_claudelog_pane
// (Phase 24.B) walkers were removed alongside the ClaudeChat /
// ClaudeLog pane kinds. The browser walker stays (active feature);
// claude_log_pane_set in claude_log.rs was also removed.

// Phase 51.B1: collect_panes + collect_panes_with_kind moved to winmux-core.

// Phase 8.A: `new_kind` decides whether the spawned sibling is a terminal (default,
// inherits the existing pane's connection) or a browser (with `new_browser_url` as
// the starting page).
// Phase 53 (rebased): `new_kind` should never be Browser or
// FileManager — the frontend's split menu no longer offers them.
// Both arms remain in the match below for back-compat (older
// frontends or RPC calls still typing those strings still work but
// produce deprecated panes; the load-time migration sweeps them
// away on the next restart).
#[allow(deprecated)]
pub(crate) fn split_pane_in(
    node: LayoutNode,
    target: &str,
    dir: SplitDirection,
    new_kind: PaneKind,
    new_browser_url: Option<String>,
    // Phase 23.C: workspace-derived fallback when the source pane has
    // no connection field (FileManager / Browser / ClaudeChat). The
    // caller is responsible for pre-computing this via
    // `first_terminal_connection` + `live_ssh_connection_for_workspace`.
    // Only used when `new_kind == Terminal`. Pass None to keep the
    // legacy Local-fallback behaviour.
    workspace_terminal_fallback: Option<Connection>,
    // Phase 33: help-topic seed for the spawned pane. Only used when
    // `new_kind == Help`. Pattern mirrors `new_browser_url`.
    new_help_topic: Option<String>,
) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Pane {
            pane_id,
            pane_kind,
            connection,
            browser,
            title,
            annotation,
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
        } => {
            if pane_id == target {
                // Phase 50: extended to 5-tuple — Diff panes carry a
                // diff_source. None on a non-Diff pane stays None.
                let (new_kind_resolved, new_conn, new_browser, new_help_t, new_diff_s) =
                    match new_kind {
                    PaneKind::Terminal => {
                        // Inherit chain: source pane's own connection →
                        // workspace-level fallback (any terminal pane or
                        // live SSH session) → Local. Splitting from a
                        // FileManager / Browser pane in an SSH workspace
                        // now correctly produces another SSH terminal,
                        // not a stray local cmd.
                        let conn = connection
                            .clone()
                            .or(workspace_terminal_fallback.clone())
                            .unwrap_or(Connection::Local { shell: None });
                        (PaneKind::Terminal, Some(conn), None, None, None)
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
                        (PaneKind::Browser, None, Some(bs), None, None)
                    }
                    PaneKind::FileManager => {
                        // File-manager panes carry no per-pane state in
                        // workspaces.json — local cwd / show_hidden live in
                        // frontend signals; the right column uses whatever
                        // SSH session the workspace currently has.
                        (PaneKind::FileManager, None, None, None, None)
                    }
                    PaneKind::Help => {
                        // Phase 33: in-app help. Topic defaults to
                        // ssh-key-setup since that's the most common
                        // entry point (offered after a password-auth
                        // SSH connect).
                        let topic = new_help_topic
                            .clone()
                            .unwrap_or_else(|| "ssh-key-setup".to_string());
                        (PaneKind::Help, None, None, Some(topic), None)
                    }
                    PaneKind::Diff => {
                        // Phase 50: new Diff panes default to Working
                        // (git diff = working tree vs index). The user
                        // can switch via the source dropdown later.
                        (PaneKind::Diff, None, None, None, Some(DiffSource::Working))
                    }
                };
                let new_pane = LayoutNode::Pane {
                    pane_id: new_pane_id(),
                    pane_kind: new_kind_resolved,
                    connection: new_conn,
                    browser: new_browser,
                    title: None,
                    annotation: None,
                    // Phase 31: new pane from a split inherits from the
                    // workspace by default (None = inherit). User can
                    // override later via pane_set_identity.
                    color: None,
                    emoji: None,
                    help_topic: new_help_t,
                    diff_source: new_diff_s,
                    smart_bidi: None,
                };
                let original = LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title,
                    annotation,
                    // Phase 31: preserve the original pane's identity
                    // across the split — it's the same logical pane,
                    // just relocated under a new Split node.
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
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
                        color,
                        emoji,
                        help_topic,
                        diff_source,
                        smart_bidi,
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
                workspace_terminal_fallback.clone(),
                new_help_topic.clone(),
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
            let (new_second, found2) = split_pane_in(
                *second,
                target,
                dir,
                new_kind,
                new_browser_url,
                workspace_terminal_fallback,
                new_help_topic,
            );
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
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
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
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
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
            color,
            emoji,
            help_topic,
            diff_source,
            smart_bidi,
        } => {
            if pane_id == target {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title: new_title.unwrap_or(title),
                    annotation: new_annotation.unwrap_or(annotation),
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                }
            } else {
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection,
                    browser,
                    title,
                    annotation,
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
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

/// Phase 61: Smart Connect (mode="cmd"/"claude") injection script, shaped
/// for the pane's shell. POSIX keeps the historical `exec` form (the
/// launched process replaces the shell); PowerShell and Cmd have no
/// `exec`, so the command runs in the shell and the user gets the prompt
/// back when it exits. Returns "" when there is nothing to inject.
fn build_smart_connect_script(
    kind: ShellKind,
    mode: &str,
    cwd_override: Option<&str>,
    cmd: Option<&str>,
    claude_args: Option<&str>,
) -> String {
    let run = match mode {
        "cmd" => match cmd {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => return String::new(),
        },
        "claude" => {
            let args = claude_args.unwrap_or("").trim();
            if args.is_empty() {
                "claude".to_string()
            } else {
                format!("claude {args}")
            }
        }
        _ => return String::new(),
    };
    let cwd = cwd_override.filter(|s| !s.is_empty());
    match kind {
        ShellKind::Posix => {
            let mut s = String::new();
            if let Some(d) = cwd {
                s.push_str(&format!("cd {} && ", shell_quote(d)));
            }
            format!("{s}exec {run}\r\n")
        }
        ShellKind::PowerShell => {
            let mut s = String::new();
            if let Some(d) = cwd {
                // -LiteralPath: no wildcard expansion on [brackets] in paths.
                s.push_str(&format!(
                    "Set-Location -LiteralPath '{}'; ",
                    d.replace('\'', "''")
                ));
            }
            format!("{s}{run}\r\n")
        }
        ShellKind::Cmd => {
            let mut s = String::new();
            if let Some(d) = cwd {
                // `/d` switches drive too. cmd can't escape `"` inside a
                // quoted arg — strip quotes/newlines instead.
                let clean = d.replace(['"', '\n', '\r'], "");
                s.push_str(&format!("cd /d \"{clean}\" && "));
            }
            format!("{s}{run}\r\n")
        }
    }
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

/// Phase 23.I: derive a tmux session name from a user-supplied pane
/// title. Keeps Unicode (Hebrew, Arabic, CJK, etc.) so a title like
/// "מחקר X" becomes a session literally named "מחקר_X". The only
/// substitutions are tmux's hard-blockers — `.` and `:` — plus
/// whitespace collapsing. Returns None when the title is empty or
/// becomes empty after sanitization; the caller falls back to the
/// pane-id-derived name in that case.
pub(crate) fn sanitize_tmux_session_name_for_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_was_underscore = false;
    for c in trimmed.chars() {
        let replaced = if c == '.' || c == ':' || c.is_whitespace() {
            '_'
        } else {
            c
        };
        if replaced == '_' {
            // Collapse runs of underscores (from whitespace runs) to one.
            if prev_was_underscore {
                continue;
            }
            prev_was_underscore = true;
        } else {
            prev_was_underscore = false;
        }
        out.push(replaced);
    }
    // Trim leading/trailing underscores left over from the trim+replace.
    let trimmed_out = out.trim_matches('_').to_string();
    if trimmed_out.is_empty() {
        return None;
    }
    // Cap at 100 chars by char (not byte) count so we don't slice
    // mid-codepoint on Hebrew/Arabic/CJK titles.
    if trimmed_out.chars().count() > 100 {
        let truncated: String = trimmed_out.chars().take(100).collect();
        Some(truncated)
    } else {
        Some(trimmed_out)
    }
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
                    let _ = ssh.try_send(SshCmd::Data(bytes));
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

fn emit_data(
    app: &AppHandle,
    session_id: &str,
    bytes: &[u8],
    leftover: &mut Vec<u8>,
    // Phase 35 (#1.2): OSC-notification side channel. The parser
    // observes the RAW bytes (OSC sequences are ASCII, so this is
    // independent of the utf8 reassembly below) and emits an
    // `osc-notification` event per detected sequence. The byte stream
    // forwarded to xterm.js is untouched.
    pane_id: &str,
    osc: &mut osc_notify::OscNotifyParser,
    // Phase 52 (BiDi 33B): per-pane bidi filter map. When the pane's
    // smart_bidi toggle is on, the chunk passes through `apply_to_pane`
    // before being decoded as UTF-8 and emitted. When off, this is a
    // memcpy (filter.enabled = false fast-path) and the bytes flow
    // through unchanged.
    bidi_filters: &bidi_filter::BidiFilterMap,
) {
    for n in osc.feed(bytes) {
        let _ = app.emit(
            "osc-notification",
            serde_json::json!({
                "pane_id": pane_id,
                "title": n.title,
                "body": n.body,
                "kind": n.kind.as_str(),
            }),
        );
    }

    // Phase 52: optional bidi rewrite. Operates on raw bytes BEFORE
    // UTF-8 reassembly so the filter's escape-sequence state machine
    // sees ANSI/CSI/OSC/DCS verbatim. The filter is itself a no-op
    // when smart_bidi is off for this pane.
    let filtered = bidi_filter::apply_to_pane(bidi_filters, pane_id, bytes);

    leftover.extend_from_slice(&filtered);
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
    // Phase 65 (J experiment): nudge CLIs (Claude Code) to emit OSC 8
    // hyperlinks so the terminal's linkHandler can make file links
    // clickable. CommandBuilder inherits the parent env, so these only
    // add/override. Engineer-only log (Rule #9). If this doesn't make
    // Claude emit OSC 8, J falls back to plan B (regex on visible text).
    cmd.env("FORCE_HYPERLINK", "1");
    cmd.env("FORCE_HYPERLINKS", "1");
    cmd.env("CLAUDE_CODE_FORCE_HYPERLINKS", "1");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");
    tracing::debug!("spawn_local_pty[{pane_id}]: injected hyperlink env vars");
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
    let sessions_for_thread = state.core.sessions.clone();
    let pane_sessions_for_thread = state.core.pane_sessions.clone();
    let bidi_for_thread = state.bidi_filters.clone();
    thread::spawn(move || {
        let mut leftover: Vec<u8> = Vec::new();
        let mut osc = osc_notify::OscNotifyParser::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => emit_data(
                    &app_for_thread,
                    &id_for_thread,
                    &buf[..n],
                    &mut leftover,
                    &pane_for_thread,
                    &mut osc,
                    &bidi_for_thread,
                ),
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

    state.core.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Local(LocalSession {
            writer,
            master: pair.master,
            killer,
        }),
    );
    Ok(id)
}

// Phase 51.B2: KnownHost + KnownHostsFile + load_known_hosts +
// save_known_hosts + iso_now + HostCheckOutcome + SshClient + impl
// Handler all moved to winmux-core. Only the symbols referenced from
// outside winmux-core (HostCheckOutcome default + SshClient itself)
// need re-exporting; the rest stay internal to the new crate.
pub(crate) use winmux_core::{HostCheckOutcome, SshClient};

// Phase 51.B2: SshClient + impl Handler moved to winmux-core
// (re-exported above). The construction sites below now pass a
// `bridge_spawner: Some(Arc::new(tunnel::spawn_bridge))` to plug the
// real tunnel impl into the russh handler without making winmux-core
// depend on tunnel.

// Phase 51.H: SSH auth primitives moved to winmux-ssh. Re-exported
// below so existing crate::pkwh / crate::pkwh_pub / crate::AuthMethod /
// crate::try_authenticate / crate::try_agent_auth / crate::key_load_needs_passphrase
// callsites resolve unchanged.
#[allow(unused_imports)]
pub(crate) use winmux_ssh::{
    key_load_needs_passphrase, pkwh, pkwh_pub, try_agent_auth, try_authenticate, AuthMethod,
};


// ─── Phase 32.B: SSH key offer + install ─────────────────────────────────

/// Path of the winmux-managed private key for a workspace.
fn winmux_key_path(workspace_id: &str) -> Result<PathBuf, String> {
    let mut p = config_dir()?;
    p.push("keys");
    std::fs::create_dir_all(&p).map_err(|e| format!("create {:?}: {e}", p))?;
    p.push(format!("{workspace_id}.key"));
    Ok(p)
}

/// True if the workspace already has a winmux-managed private key on
/// disk — we don't re-offer in that case.
fn winmux_managed_key_exists(workspace_id: &str) -> bool {
    winmux_key_path(workspace_id)
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[tauri::command]
async fn ssh_key_offer_dismiss(
    state: State<'_, AppState>,
    app: AppHandle,
    dont_show_again: bool,
) -> Result<(), String> {
    if dont_show_again {
        {
            let mut s = state.settings.lock().map_err(|e| e.to_string())?;
            s.ssh_key_offer_disabled = true;
        }
        // Phase 59.E: was a direct std::fs::write — non-atomic
        // (violates Absolute Rule #7) and error-swallowing. Use the
        // settings module's tmp+rename save; failure is logged, not
        // raised (a failed "don't show again" persist shouldn't fail
        // the dismiss itself — worst case the offer pops once more).
        if let Ok(snapshot) = state.settings.lock().map(|s| s.clone()) {
            if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                dlog(&format!("ssh_key_offer_dismiss: settings save failed: {e}"));
            }
        }
        let _ = app.emit("settings:changed", ());
    }
    Ok(())
}

#[tauri::command]
async fn ssh_key_generate_and_install(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    ssh_user: String,
    ssh_host: String,
    ssh_port: u16,
    password: String,
    dont_show_again: bool,
) -> Result<String, String> {
    let _ = pane_id;
    let priv_path = winmux_key_path(&workspace_id)?;
    let pub_path: PathBuf = {
        let mut p = priv_path.clone();
        let mut s = p.file_name().unwrap().to_os_string();
        s.push(".pub");
        p.set_file_name(s);
        p
    };

    // 1) Generate ed25519 keypair via ssh-keygen.exe (ships with
    //    Windows 10+ OpenSSH). Same approach as the provisioning
    //    wizard's GenerateKeypair step.
    if priv_path.exists() {
        std::fs::remove_file(&priv_path).map_err(|e| format!("remove old key: {e}"))?;
    }
    if pub_path.exists() {
        std::fs::remove_file(&pub_path).map_err(|e| format!("remove old pubkey: {e}"))?;
    }
    let priv_str = priv_path.to_string_lossy().to_string();
    let out = tokio::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            &format!("winmux-{workspace_id}"),
            "-f",
            &priv_str,
        ])
        .output()
        .await
        .map_err(|e| format!("spawn ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let pub_line =
        std::fs::read_to_string(&pub_path).map_err(|e| format!("read pubkey: {e}"))?;
    let pub_line_trim = pub_line.trim();

    // 2) Open a fresh SSH session using the password that just worked
    //    (the original handle isn't easily reusable here — opening a
    //    new short-lived one is simpler and the user already typed
    //    the password once for this flow).
    let target = format!("{ssh_host}:{ssh_port}");
    // Phase 38: keepalive (see spawn_ssh) — short-lived key-install
    // session, but keep it consistent with the rest.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let mut handle = client::connect(
        config,
        (ssh_host.as_str(), ssh_port),
        SshClient::new_anonymous(target.clone()),
    )
    .await
    .map_err(|e| format!("ssh connect {target}: {e}"))?;
    let ok = handle
        .authenticate_password(&ssh_user, &password)
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !ok {
        return Err("authentication failed (password rejected)".into());
    }

    // 3) Append the public key to ~/.ssh/authorized_keys. No sudo —
    //    writes only to the user's own home, so this works even for
    //    a non-root user with no sudo at all.
    let install_cmd = format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && \
         touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && \
         (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys)",
        key = pub_line_trim.replace('\'', "'\\''"),
    );
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, install_cmd.as_str())
        .await
        .map_err(|e| e.to_string())?;
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
    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&out_buf).to_string();
        return Err(format!(
            "install pubkey failed (exit {exit_code}): {stderr}"
        ));
    }

    // 4) Update the workspace's stored Connection — switch from
    //    password to key. The next pane_connect will use the key path
    //    and skip the password prompt.
    {
        let mut file = state.workspaces.lock().map_err(|e| e.to_string())?;
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            // The Connection::Ssh variant has no `password` field —
            // passwords are transient (passed per-connect, not
            // persisted). Setting the key_path is all that's needed
            // so future pane_connect calls use the key.
            if let Some(Connection::Ssh { key_path: kp, .. }) = ws.connection.as_mut() {
                *kp = Some(priv_str.clone());
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());

    // 5) Persist "don't show again" if requested.
    if dont_show_again {
        {
            let mut s = state.settings.lock().map_err(|e| e.to_string())?;
            s.ssh_key_offer_disabled = true;
        }
        // Phase 59.E: atomic tmp+rename via the settings module
        // (was a direct std::fs::write — Rule #7 violation). Failure
        // logged, not raised: the key IS installed at this point and
        // the workspace persisted; a failed flag save just means the
        // offer may pop once more.
        if let Ok(snapshot) = state.settings.lock().map(|s| s.clone()) {
            if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                dlog(&format!(
                    "ssh_key_generate_and_install: settings save failed: {e}"
                ));
            }
        }
        let _ = app.emit("settings:changed", ());
    }

    dlog(&format!(
        "ssh_key_generate_and_install: installed key for ws={workspace_id} user={ssh_user} host={ssh_host}"
    ));
    Ok(priv_str)
}

/// Phase 56-B: "Connect to existing server" provisioning shortcut.
///
/// The user already has an account on a remote server; they just want
/// winmux to:
///   1. Open a one-shot SSH session with their password,
///   2. Generate an ed25519 keypair (stored at
///      `%APPDATA%\winmux\keys\<workspace_id>.key`),
///   3. Append the pubkey to `~/.ssh/authorized_keys` on the remote,
///   4. Verify the key handshake works,
///   5. Persist a fresh workspace with the key path baked in.
///
/// The password is consumed in-memory only — never written to disk.
/// On any failure between steps 2 and 5 the partial keypair on disk
/// is left in place (the next attempt overwrites it); the workspace
/// is only persisted on a fully clean run.
///
/// Returns the new workspace_id so the frontend can switch to it.
#[tauri::command]
async fn provision_existing_install_key(
    state: State<'_, AppState>,
    app: AppHandle,
    host: String,
    port: u16,
    ssh_user: String,
    password: String,
    workspace_name: String,
) -> Result<String, String> {
    if host.is_empty() {
        return Err("host is required".into());
    }
    if ssh_user.is_empty() {
        return Err("user is required".into());
    }
    if password.is_empty() {
        return Err("password is required".into());
    }
    let workspace_id = new_workspace_id();

    // Compute key paths up front + clear any stale leftovers from a
    // previous attempt with the same (yet-to-be-persisted) id.
    let priv_path = winmux_key_path(&workspace_id)?;
    let pub_path: PathBuf = {
        let mut p = priv_path.clone();
        let mut s = p
            .file_name()
            .ok_or_else(|| "winmux_key_path: no file name".to_string())?
            .to_os_string();
        s.push(".pub");
        p.set_file_name(s);
        p
    };
    if priv_path.exists() {
        std::fs::remove_file(&priv_path).map_err(|e| format!("remove old key: {e}"))?;
    }
    if pub_path.exists() {
        std::fs::remove_file(&pub_path).map_err(|e| format!("remove old pubkey: {e}"))?;
    }
    let priv_str = priv_path.to_string_lossy().to_string();

    // 1) Generate the ed25519 keypair via the system ssh-keygen (ships
    //    with Windows 10+ OpenSSH; same call shape as the wizard's
    //    GenerateKeypair step + ssh_key_generate_and_install).
    let out = tokio::process::Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            &format!("winmux-{workspace_id}"),
            "-f",
            &priv_str,
        ])
        .output()
        .await
        .map_err(|e| format!("spawn ssh-keygen: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let pub_line =
        std::fs::read_to_string(&pub_path).map_err(|e| format!("read pubkey: {e}"))?;
    let pub_line_trim = pub_line.trim().to_string();

    // 2) Connect with the password to validate creds + install the key.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let target = format!("{host}:{port}");
    let mut handle = client::connect(
        config.clone(),
        (host.as_str(), port),
        SshClient::new_anonymous(target.clone()),
    )
    .await
    .map_err(|e| format!("ssh connect {target}: {e}"))?;
    let ok = handle
        .authenticate_password(&ssh_user, &password)
        .await
        .map_err(|e| format!("authenticate: {e}"))?;
    if !ok {
        return Err("authentication failed (password rejected)".into());
    }

    // 3) Append pubkey to ~/.ssh/authorized_keys. Idempotent (grep -qxF).
    let install_cmd = format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && \
         touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && \
         (grep -qxF '{key}' ~/.ssh/authorized_keys || echo '{key}' >> ~/.ssh/authorized_keys)",
        key = pub_line_trim.replace('\'', "'\\''"),
    );
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, install_cmd.as_str())
        .await
        .map_err(|e| e.to_string())?;
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
    if exit_code != 0 {
        let stderr = String::from_utf8_lossy(&out_buf).to_string();
        return Err(format!(
            "install pubkey failed (exit {exit_code}): {stderr}"
        ));
    }
    // Drop the password-authenticated handle.
    drop(handle);

    // 4) Verify reconnect with the new key. Surfaces a clear error if
    //    sshd's PubkeyAuthentication is off or AuthorizedKeysFile is
    //    pointed somewhere unusual — before we persist a workspace
    //    that wouldn't work.
    let verify = connect_and_authenticate(
        &host,
        &ssh_user,
        port,
        Some(&priv_str),
        None,
        None,
        false,
    )
    .await
    .map_err(|e| format!("verify key: {e}"))?;
    drop(verify);

    // 5) Persist a fresh workspace. Connection mirrors workspace_create.
    let conn = Connection::Ssh {
        host: host.clone(),
        user: ssh_user.clone(),
        port,
        key_path: Some(priv_str.clone()),
    };
    let final_name = if workspace_name.trim().is_empty() {
        host.clone()
    } else {
        workspace_name.trim().to_string()
    };
    let ws = Workspace {
        id: workspace_id.clone(),
        name: final_name,
        color: None,
        emoji: None,
        cwd: None,
        connection: Some(conn.clone()),
        layout: Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(conn),
            browser: None,
            title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }),
        setup_command: None,
        teardown_command: None,
        env: Vec::new(),
        auto_port_forward: false,
        last_active_at: 0,
        git_worktree: None,
    };
    {
        let mut file = state.workspaces.lock().map_err(|e| e.to_string())?;
        file.active_workspace_id = Some(workspace_id.clone());
        file.workspaces.push(ws);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());

    dlog(&format!(
        "provision_existing_install_key: created ws={workspace_id} host={host} user={ssh_user}"
    ));
    Ok(workspace_id)
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

/// Phase 41: result of the connect→host-key→authenticate handshake,
/// factored out of `spawn_ssh` so `workspace_ensure_connected` can
/// establish a reusable handle without a pane (no PTY / tmux / bootstrap
/// / reverse-tunnel — the caller owns those).
struct SshHandshake {
    handle: client::Handle<SshClient>,
    auth_method: AuthMethod,
    /// The reverse-tunnel HMAC token baked into the connection's handler.
    /// `spawn_ssh` forwards it to the remote for the CLI dial-back; the
    /// headless path ignores it.
    tunnel_token: Arc<String>,
}

/// Phase 41: connect to the SSH target, run the host-key check, and
/// authenticate. Shared by `spawn_ssh` (pane path) and
/// `workspace_ensure_connected` (headless background path). Surfaces the
/// same `UNKNOWN_HOST` / `HOST_KEY_MISMATCH` / auth-failure errors as
/// before. Includes the Phase 38 keepalive so headless handles also
/// survive idle NAT timeouts.
async fn connect_and_authenticate(
    host: &str,
    user: &str,
    port: u16,
    key_path: Option<&str>,
    key_passphrase: Option<&str>,
    password: Option<&str>,
    accept_unknown_host: bool,
) -> Result<SshHandshake, String> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let target = format!("{}:{}", host, port);
    let outcome_arc = Arc::new(Mutex::new(HostCheckOutcome::default()));
    let token = Arc::new(tunnel::generate_token());
    let sh = SshClient {
        target: target.clone(),
        accept_unknown: accept_unknown_host,
        result: outcome_arc.clone(),
        tunnel_token: Some(token.clone()),
        // Phase 51.B2 option β: inject the tunnel::spawn_bridge fn so
        // winmux-core's Handler impl can fire it on forwarded-tcpip
        // without taking a static dep on the tunnel module.
        bridge_spawner: Some(std::sync::Arc::new(tunnel::spawn_bridge)),
    };

    dlog(&format!("ssh.connect: client::connect to {} starting", target));
    let connect_res = client::connect(config, (host, port), sh).await;
    dlog(&format!(
        "ssh.connect: client::connect to {} returned (ok={})",
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

    let auth_method = try_authenticate(&mut handle, user, key_path, key_passphrase, password).await?;
    let auth_method = match auth_method {
        Some(m) => m,
        None => return Err("authentication failed (agent, key, and password all failed)".into()),
    };

    Ok(SshHandshake {
        handle,
        auth_method,
        tunnel_token: token,
    })
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
    // Phase 23.F: when set, override the pane-id-derived tmux session
    // name. Passed through from pane_connect when the picker UI chose
    // a specific orphan session to attach to.
    tmux_session_name: Option<String>,
) -> Result<String, String> {
    dlog(&format!(
        "spawn_ssh: entry ws={} pane={} target={}@{}:{}",
        workspace_id, pane_id, user, host, port
    ));
    // Phase 41: connect + host-key + auth now live in the shared
    // `connect_and_authenticate` helper (includes the Phase 38 keepalive).
    dlog("spawn_ssh: connect_and_authenticate begin");
    let SshHandshake {
        mut handle,
        auth_method,
        tunnel_token: token,
    } = connect_and_authenticate(
        &host,
        &user,
        port,
        key_path.as_deref(),
        key_passphrase.as_deref(),
        password.as_deref(),
        accept_unknown_host,
    )
    .await?;
    dlog(&format!("spawn_ssh: authenticated method={auth_method:?}"));

    // Phase 32.B: offer to convert a password-auth connection to key
    // auth. Skipped when the user previously ticked "don't show again",
    // when auth already uses a key/agent, or when the workspace
    // already has a winmux-managed key on disk for this user@host.
    if auth_method == AuthMethod::Password {
        let suppressed = state
            .settings
            .lock()
            .ok()
            .map(|s| s.ssh_key_offer_disabled)
            .unwrap_or(false);
        if !suppressed && !winmux_managed_key_exists(&workspace_id) {
            let _ = app.emit(
                "ssh-key-offer",
                serde_json::json!({
                    "workspace_id": workspace_id,
                    "pane_id": pane_id,
                    "ssh_user": user,
                    "ssh_host": host,
                    "ssh_port": port,
                }),
            );
        }
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

    // Phase 6.3 → 47.A: ask server to forward a port back to us. With
    // port=0 the server picks a free one and returns it. Forwarded
    // channels arrive in our Handler's `server_channel_open_forwarded_tcpip`
    // and get bridged to the local pipe. Phase 47.A factored this into
    // `setup_workspace_reverse_tunnel` so the headless connect path can
    // call the same setup — that helper also fires `spawn_port_watcher`.
    let remote_port =
        setup_workspace_reverse_tunnel(state, &mut handle, &workspace_id, &token).await;

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

    // Phase 47.A: the watcher launch moved into
    // `setup_workspace_reverse_tunnel` above so the headless connect
    // path gets it too. Dedup via state.core.port_watchers still applies.

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

    // Phase 65 (J experiment): try to make remote CLIs (Claude Code)
    // emit OSC 8 hyperlinks. Best-effort — sshd's AcceptEnv may filter
    // these out, in which case J falls back to plan B (regex on the
    // visible `[file] …` text). Engineer-only log (Rule #9).
    for (k, v) in [
        ("FORCE_HYPERLINK", "1"),
        ("FORCE_HYPERLINKS", "1"),
        ("CLAUDE_CODE_FORCE_HYPERLINKS", "1"),
        ("COLORTERM", "truecolor"),
    ] {
        let _ = channel.set_env(false, k, v.to_string()).await;
    }
    tracing::debug!("spawn_ssh[{pane_id}]: requested hyperlink env vars (best-effort)");

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

    // Phase 18: hooks-outdated probe. Fire-and-forget — never blocks
    // the SSH bring-up. Compares the version stamped into the
    // remote's ~/.claude/settings.json (under
    // `winmux_meta.hooks_version`) with the manifest's
    // `hooks.claude-code.version`. When the remote is older AND the
    // user hasn't dismissed that version, emit `hooks:outdated` so
    // the frontend banner appears.
    {
        let app_clone = app.clone();
        let state_clone: AppState = (*state).clone();
        let ws_id = workspace_id.clone();
        let pane_id_clone = pane_id.clone();
        let handle_for_hooks = Arc::clone(&handle_arc);
        tauri::async_runtime::spawn(async move {
            crate::updater::check_remote_hooks(
                &state_clone,
                &app_clone,
                &handle_for_hooks,
                &ws_id,
                &pane_id_clone,
            )
            .await;
        });
    }

    let id_for_task = id.clone();
    let pane_for_task = pane_id.clone();
    let app_for_task = app.clone();
    let sessions_for_task = state.core.sessions.clone();
    let pane_sessions_for_task = state.core.pane_sessions.clone();
    let forwards_for_task = state.core.forwards.clone();
    let workspace_for_task = workspace_id.clone();
    // Phase 39: clean up this session's reverse-tunnel remote port from
    // the internal-ports set when the session ends.
    let internal_ports_for_task = state.core.internal_reverse_tunnel_remote_ports.clone();
    let reverse_port_for_task = remote_port as u16;
    let bidi_for_task = state.bidi_filters.clone();
    tokio::spawn(async move {
        let mut leftover: Vec<u8> = Vec::new();
        let mut osc = osc_notify::OscNotifyParser::new();
        let mut exit_reason: Option<String> = None;
        // Phase 38: track last inbound data so disconnect logs carry a
        // "how long was it idle before dropping" age — distinguishes a
        // keepalive/NAT timeout (long idle) from an active-session drop.
        let mut last_data_at = std::time::Instant::now();
        // Phase 38: stable ids for the disconnect log line.
        let ch_id = channel.id();
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { data }) => {
                            last_data_at = std::time::Instant::now();
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut leftover, &pane_for_task, &mut osc, &bidi_for_task);
                        }
                        Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                            last_data_at = std::time::Instant::now();
                            emit_data(&app_for_task, &id_for_task, &data[..], &mut leftover, &pane_for_task, &mut osc, &bidi_for_task);
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            exit_reason = Some(format!("exit {exit_status}"));
                        }
                        Some(ChannelMsg::Eof) => {
                            dlog(&format!(
                                "ssh-disconnect: clean Eof, workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
                            break;
                        }
                        Some(ChannelMsg::Close) => {
                            dlog(&format!(
                                "ssh-disconnect: clean Close, workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
                            break;
                        }
                        None => {
                            dlog(&format!(
                                "ssh-disconnect: transport dropped (likely network/keepalive timeout), workspace={} pane={} channel={:?} last_activity_ms={}",
                                workspace_for_task, pane_for_task, ch_id, last_data_at.elapsed().as_millis()
                            ));
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
        // Phase 39: drop this session's reverse-tunnel remote port from
        // the internal-ports set.
        if reverse_port_for_task != 0 {
            if let Ok(mut m) = internal_ports_for_task.lock() {
                if let Some(set) = m.get_mut(&workspace_for_task) {
                    set.remove(&reverse_port_for_task);
                    if set.is_empty() {
                        m.remove(&workspace_for_task);
                    }
                }
            }
        }
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

    // Phase 23.F: caller-supplied name wins (picker path); pane-id
    // fallback keeps the legacy auto-name behaviour.
    let tmux_name = if persistent {
        Some(
            tmux_session_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| sanitize_tmux_session_name(&pane_id)),
        )
    } else {
        None
    };
    state.core.sessions.lock().unwrap().insert(
        id.clone(),
        Session::Ssh(SshSession {
            tx: Some(tx),
            handle: handle_for_state,
            workspace_id: workspace_id_for_state.clone(),
            tmux_session: tmux_name.clone(),
            host: host.clone(),
            user: user.clone(),
            port,
            key_path: key_path.clone(),
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
        let sessions_clone = state.core.sessions.clone();
        let id_clone = id.clone();
        let name_clone = name.clone();
        let socket_addr = if remote_port != 0 {
            format!("127.0.0.1:{}", remote_port)
        } else {
            String::new()
        };
        let token_clone = token.as_str().to_string();
        let pane_for_exec = pane_id.clone();
        // Phase tmux-conf: read the user's setting BEFORE we hand
        // control to the spawned task (state.settings is not Send-
        // safe to hold across await points). Default true so users
        // who never touched Settings → Terminal get the bundled
        // scrollback-friendly behaviour out of the box.
        let use_winmux_tmux_conf = state
            .settings
            .lock()
            .ok()
            .map(|s| s.terminal.use_winmux_tmux_config)
            .unwrap_or(true);
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
            // Phase tmux-conf: when enabled, point tmux at our bundled
            // conf via `-f ~/.winmux/tmux.conf`. Falls through to the
            // user's own ~/.tmux.conf if the file is absent (tmux
            // logs a warning and uses defaults — non-fatal). When the
            // setting is off, omit -f so the user's conf alone applies.
            let tmux_flags = if use_winmux_tmux_conf {
                "-f $HOME/.winmux/tmux.conf "
            } else {
                ""
            };
            script.push_str(&format!(
                "command -v tmux >/dev/null 2>&1 && exec tmux {flags}new-session -A -s {name} || echo '[winmux] tmux not installed on remote — falling back to plain shell'\r\n",
                flags = tmux_flags,
                name = shell_quote(&name_clone)
            ));
            let mut sessions = sessions_clone.lock().unwrap();
            if let Some(Session::Ssh(ssh)) = sessions.get_mut(&id_clone) {
                let _ = ssh.try_send(SshCmd::Data(script.into_bytes()));
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
            let _ = ssh.try_send(SshCmd::Kill);
        }
    }
}

// ─── Phase 8.B: SSH local port forwards ─────────────────────────────────────

// Find an SSH handle for the workspace by walking its connected terminal panes.
// Returns the first one found, or None if no terminal pane in the workspace
// currently has an active SSH session.
/// Phase 47.A: workspace-level reverse-tunnel setup, factored out of
/// `spawn_ssh` so the headless `workspace_ensure_connected` path can
/// call it too. Without this, a workspace whose toggle is on but with
/// no terminal pane open never got a tunnel — so the watcher couldn't
/// dial back, and PortsWindow stayed "stuck searching."
///
/// Does the workspace-level slice ONLY: `tcpip_forward` (kernel picks
/// a free remote port), records the port + token in `AppState`, and
/// fires `spawn_port_watcher` (deduped). Pane-specific bits — the
/// env-file write that takes `&pane_id`, and the `WINMUX_PANE_ID`
/// `set_env` on the shell channel — stay in `spawn_ssh`.
///
/// Returns the assigned remote port, or 0 if `tcpip_forward` failed
/// (which still leaves the SSH handle usable for tmux-list / file
/// manager — just no detection).
async fn setup_workspace_reverse_tunnel(
    state: &AppState,
    handle: &mut client::Handle<SshClient>,
    workspace_id: &str,
    token: &Arc<String>,
) -> u16 {
    let remote_port = match handle.tcpip_forward("127.0.0.1", 0).await {
        Ok(p) => {
            dlog(&format!(
                "setup_workspace_reverse_tunnel[{workspace_id}]: tcpip_forward got remote port {p}"
            ));
            p as u16
        }
        Err(e) => {
            dlog(&format!(
                "setup_workspace_reverse_tunnel[{workspace_id}]: tcpip_forward failed: {e}"
            ));
            tracing::warn!("tcpip_forward[{workspace_id}] failed: {e}");
            return 0;
        }
    };
    if remote_port == 0 {
        return 0;
    }
    // Phase 39: record winmux's own reverse-tunnel remote port so the
    // auto-port watcher skips it (it's an HMAC endpoint).
    state.core
        .internal_reverse_tunnel_remote_ports
        .lock()
        .unwrap()
        .entry(workspace_id.to_string())
        .or_default()
        .insert(remote_port);
    // Phase 47: stash the tunnel token so a later
    // workspace_ensure_port_watcher can spawn the watcher without
    // having to rebuild the SSH session.
    state.core
        .workspace_tunnel_tokens
        .lock()
        .unwrap()
        .insert(workspace_id.to_string(), token.clone());
    // Phase 47.A: best-effort watcher launch as part of tunnel setup.
    // spawn_port_watcher dedups via port_watchers so calling here AND
    // from try_ensure_port_watcher later is safe.
    let _ = spawn_port_watcher(state, handle, workspace_id, remote_port, token).await;
    remote_port
}

/// Phase 47: spawn the remote `winmux port-watch` for a workspace.
/// Deduplicated via `state.core.port_watchers` — calling twice in a row is
/// a no-op the second time. Stores the spawned task's JoinHandle in
/// `state.core.port_watcher_tasks` so toggling detection off can `.abort()`
/// it. Returns Err on channel/exec failure; on success the task
/// detaches and the watcher streams events back through the reverse
/// tunnel (dispatched by `port.opened` / `port.closed` in rpc_server).
async fn spawn_port_watcher(
    state: &AppState,
    handle: &client::Handle<SshClient>,
    workspace_id: &str,
    remote_port: u16,
    token: &Arc<String>,
) -> Result<(), String> {
    // Dedup: if a watcher's already running for this workspace, no-op.
    {
        let mut set = state.core.port_watchers.lock().unwrap();
        if set.contains(workspace_id) {
            return Ok(());
        }
        set.insert(workspace_id.to_string());
    }
    let mut wchan = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            dlog(&format!("port-watch[{workspace_id}]: channel_open_session failed: {e}"));
            state.core.port_watchers.lock().unwrap().remove(workspace_id);
            return Err(format!("channel_open: {e}"));
        }
    };
    let socket_addr = format!("127.0.0.1:{}", remote_port);
    let _ = wchan.set_env(false, "WINMUX_SOCKET_ADDR", socket_addr).await;
    let _ = wchan
        .set_env(false, "WINMUX_TUNNEL_TOKEN", token.as_str().to_string())
        .await;
    // Exec channels don't source the rc files that add ~/.winmux/bin to PATH,
    // so use the explicit path.
    let cmd = format!(
        "\"$HOME/.winmux/bin/winmux\" port-watch --workspace {}",
        shell_quote(workspace_id)
    );
    if let Err(e) = wchan.exec(true, cmd.as_str()).await {
        dlog(&format!("port-watch[{workspace_id}]: exec failed: {e}"));
        state.core.port_watchers.lock().unwrap().remove(workspace_id);
        return Err(format!("exec failed: {e}"));
    }
    let ws_guard = workspace_id.to_string();
    let watchers = state.core.port_watchers.clone();
    let tasks = state.core.port_watcher_tasks.clone();
    let task = tokio::spawn(async move {
        loop {
            match wchan.wait().await {
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        watchers.lock().unwrap().remove(&ws_guard);
        tasks.lock().unwrap().remove(&ws_guard);
        dlog(&format!(
            "port-watch[{ws_guard}]: channel closed, watcher slot freed"
        ));
    });
    state.core
        .port_watcher_tasks
        .lock()
        .unwrap()
        .insert(workspace_id.to_string(), task);
    dlog(&format!(
        "port-watch[{workspace_id}]: launched (remote_port={remote_port})"
    ));
    Ok(())
}

/// Phase 47: abort the watcher task + clear the workspace's detected
/// ports, and tell the FE to wipe its list. Idempotent — safe to call
/// when no watcher is running.
fn clear_workspace_detection(state: &AppState, app: &AppHandle, workspace_id: &str) {
    let aborted = {
        let mut tasks = state.core.port_watcher_tasks.lock().unwrap();
        tasks.remove(workspace_id).map(|h| {
            h.abort();
            true
        })
    };
    if aborted.is_some() {
        state.core.port_watchers.lock().unwrap().remove(workspace_id);
    }
    state.core
        .detected_ports
        .lock()
        .unwrap()
        .remove(workspace_id);
    let _ = app.emit(
        "port-detection-cleared",
        serde_json::json!({ "workspace_id": workspace_id }),
    );
    dlog(&format!(
        "port-watch[{workspace_id}]: detection cleared (was_running={})",
        aborted.is_some()
    ));
}

fn find_ssh_handle_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Arc<client::Handle<SshClient>>> {
    let sessions = state.core.sessions.lock().unwrap();
    for s in sessions.values() {
        if let Session::Ssh(ssh) = s {
            if ssh.workspace_id == workspace_id {
                return Some(Arc::clone(&ssh.handle));
            }
        }
    }
    None
}

/// Phase 36 (#2.2) → 36.A: open an auto-forward for a remote listening
/// port. We bind `127.0.0.1:0` and let the kernel hand us a free
/// ephemeral port — the user reaches the server at whatever local port
/// that is (shown in the Ports panel). This is simpler and race-free vs
/// trying to match the remote port: no +1..+9 fallback, no cross-
/// workspace collision when two servers both listen on :3000.
/// Idempotent on (workspace, remote_port).
pub(crate) async fn open_auto_forward(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    remote_addr: &str,
    remote_port: u16,
) -> Result<u16, String> {
    {
        let m = state.core.forwards.lock().unwrap();
        if let Some(e) = m.get(&(workspace_id.to_string(), remote_port)) {
            return Ok(e.local_port);
        }
    }
    let handle = find_ssh_handle_for_workspace(state, workspace_id)
        .ok_or_else(|| "no active SSH session for this workspace".to_string())?;

    // Bind port 0 → kernel picks a free ephemeral port (Windows ~49152+).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let ws_for_task = workspace_id.to_string();
    let forwards_for_task = state.core.forwards.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                accept = listener.accept() => {
                    let (mut sock, peer) = match accept {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let h = Arc::clone(&handle);
                    tokio::spawn(async move {
                        let chan = match h
                            .channel_open_direct_tcpip(
                                "localhost",
                                remote_port as u32,
                                peer.ip().to_string(),
                                peer.port() as u32,
                            )
                            .await
                        {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        let mut chan_stream = chan.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut chan_stream).await;
                    });
                }
            }
        }
        forwards_for_task
            .lock()
            .unwrap()
            .remove(&(ws_for_task, remote_port));
    });

    state.core.forwards.lock().unwrap().insert(
        (workspace_id.to_string(), remote_port),
        ForwardEntry {
            local_port,
            cancel: Some(cancel_tx),
        },
    );
    // Phase 62.A (item F): the local tunnel listener is loopback-only
    // (127.0.0.1) — services are reachable from this machine, never the
    // LAN/external IP. Logged explicitly so a future "it's going through
    // my external IP" report can be ruled out from the debug.log.
    dlog(&format!(
        "open_auto_forward[{}:{}]: bound 127.0.0.1:{} (loopback only, kernel-assigned)",
        workspace_id, remote_port, local_port
    ));

    // Phase 46: sanity-probe the bound local port before telling the FE
    // the forward is live. Catches the IPv4/IPv6 dual-stack pitfall and
    // any binds that look successful but aren't actually accepting yet —
    // so the user never opens a browser tab on a dead port.
    let probe_target = format!("127.0.0.1:{local_port}");
    let probe = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        tokio::net::TcpStream::connect(&probe_target),
    )
    .await;
    let probe_ok = matches!(probe, Ok(Ok(_)));
    if !probe_ok {
        let why = match probe {
            Ok(Err(e)) => format!("connect failed: {e}"),
            Err(_) => "200ms timeout".to_string(),
            Ok(Ok(_)) => unreachable!(),
        };
        dlog(&format!(
            "open_auto_forward[{}:{}]: sanity probe to {} FAILED ({why}) — tearing down",
            workspace_id, remote_port, probe_target
        ));
        close_one_forward(state, app, workspace_id, remote_port);
        return Err(format!(
            "forward bound but localhost:{local_port} unreachable ({why})"
        ));
    }
    dlog(&format!(
        "open_auto_forward[{}:{}]: sanity probe to {} OK",
        workspace_id, remote_port, probe_target
    ));
    let _ = app.emit(
        "port-forwarded",
        serde_json::json!({
            "workspace_id": workspace_id,
            "remote_addr": remote_addr,
            "remote_port": remote_port,
            "local_port": local_port,
        }),
    );
    Ok(local_port)
}

/// Phase 36: tear down a single (workspace, remote_port) forward.
pub(crate) fn close_one_forward(
    state: &AppState,
    app: &AppHandle,
    workspace_id: &str,
    remote_port: u16,
) {
    let removed = {
        let mut m = state.core.forwards.lock().unwrap();
        m.remove(&(workspace_id.to_string(), remote_port))
    };
    if let Some(mut e) = removed {
        if let Some(c) = e.cancel.take() {
            let _ = c.send(());
        }
        let _ = app.emit(
            "port-forward-stopped",
            serde_json::json!({
                "workspace_id": workspace_id,
                "remote_port": remote_port,
            }),
        );
    }
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

// Phase 23.B: does the layout contain a non-terminal pane that depends on
// a live workspace-level SSH handle? FileManager and Browser panes pull
// the SSH handle out of `state.core.sessions` at runtime via
// `pick_ssh_handle_for_workspace`; if we tear down the last terminal pane's
// SSH session, those panes go dark with no in-UI way to reconnect.
// ClaudeChat is local, doesn't count.
#[allow(deprecated)]
fn layout_has_ssh_consumer_pane(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane { pane_kind, .. } => {
            matches!(pane_kind, PaneKind::FileManager | PaneKind::Browser)
        }
        LayoutNode::Split { first, second, .. } => {
            layout_has_ssh_consumer_pane(first) || layout_has_ssh_consumer_pane(second)
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
    // Phase 23.D: workspace.connection is canonical from creation
    // onward. The first Terminal pane also carries it for
    // back-compat with older code paths that read pane.connection
    // directly; future panes added via split / programmatic add
    // inherit from the workspace level when their own field is None.
    let conn = input.connection.clone();
    let ws = Workspace {
        id: new_workspace_id(),
        name: input.name,
        color: input.color,
        emoji: None,
        cwd: input.cwd,
        connection: Some(conn.clone()),
        layout: Some(LayoutNode::Pane {
            pane_id: new_pane_id(),
            pane_kind: PaneKind::Terminal,
            connection: Some(conn),
            browser: None,
            title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }),
        setup_command: input.setup_command,
        teardown_command: input.teardown_command,
        env: input.env.unwrap_or_default(),
        auto_port_forward: false,
        last_active_at: 0,
        git_worktree: None,
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
    // Phase 37: editable connection. When present, replaces the
    // workspace's canonical connection AND rewrites every Terminal
    // pane's connection so the next reconnect uses the new host / user /
    // port / key. Absent = leave the connection untouched.
    connection: Option<Connection>,
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
        if let Some(conn) = connection {
            ws.connection = Some(conn.clone());
            if let Some(layout) = ws.layout.as_mut() {
                set_terminal_connections(layout, &conn);
            }
        }
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 37: rewrite the `connection` on every Terminal pane in the
/// layout to `conn`. Used when the user edits a workspace's connection
/// so existing panes reconnect with the new credentials. Non-terminal
/// panes (browser / file-manager / help) carry no connection — skipped.
fn set_terminal_connections(node: &mut LayoutNode, conn: &Connection) {
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } => {
            if matches!(pane_kind, PaneKind::Terminal) {
                *connection = Some(conn.clone());
            }
        }
        LayoutNode::Split { first, second, .. } => {
            set_terminal_connections(first, conn);
            set_terminal_connections(second, conn);
        }
    }
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
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        });
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

// Phase 51.B1: first_terminal_connection_pub moved to winmux-core.

/// Phase 23.C: visible to other modules (rpc_server) for the same
/// inheritance chain when splits come in via RPC.
pub(crate) fn live_ssh_connection_for_workspace_pub(
    state: &AppState,
    workspace_id: &str,
) -> Option<Connection> {
    live_ssh_connection_for_workspace(state, workspace_id)
}

// Phase 23.C: extract a `Connection` from a live SSH session for this
// workspace. Returns None if no SSH session is currently bound to the
// workspace. Used as a second-tier fallback in `workspace_split` so
// the user can re-add a terminal pane to an SSH workspace whose
// connection details no longer live in any pane (e.g. after closing
// the last terminal but the SSH handle is still alive because a
// FileManager pane kept it pinned).
fn live_ssh_connection_for_workspace(
    state: &AppState,
    workspace_id: &str,
) -> Option<Connection> {
    let sessions = state.core.sessions.lock().ok()?;
    for sess in sessions.values() {
        if let Session::Ssh(s) = sess {
            if s.workspace_id == workspace_id {
                return Some(Connection::Ssh {
                    host: s.host.clone(),
                    user: s.user.clone(),
                    port: s.port,
                    key_path: s.key_path.clone(),
                });
            }
        }
    }
    None
}

// Phase 51.B1: first_terminal_connection + backfill_terminal_connections
// moved to winmux-core.

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

// Phase 30: dedicated identity command for live preview. The full
// `workspace_update` path is still used by the modal's Save button;
// this one lets a swatch click instant-save without rebuilding the
// whole field set. Validates: hex must be `#rrggbb`, emoji must be
// <= 16 UTF-8 bytes. Returns the updated workspace.
#[tauri::command]
async fn workspace_set_identity(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    color: Option<String>,
    emoji: Option<String>,
) -> Result<Workspace, String> {
    if let Some(c) = color.as_deref() {
        let bytes = c.as_bytes();
        let ok = bytes.len() == 7
            && bytes[0] == b'#'
            && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(format!("invalid color (want #rrggbb, got {c:?})"));
        }
    }
    if let Some(e) = emoji.as_deref() {
        if e.len() > 16 {
            return Err(format!("emoji too long ({} bytes, max 16)", e.len()));
        }
    }
    let updated: Workspace;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.color = color;
        ws.emoji = emoji;
        updated = ws.clone();
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

// Phase 36 (#2.2): toggle auto port forwarding for a workspace.
// Persists the flag. When turned off, also tears down any forwards the
// watcher already opened for this workspace (the watcher keeps running
// remotely but its events are ignored — see the dispatch arms).
#[tauri::command]
async fn workspace_set_auto_port_forward(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    enabled: bool,
) -> Result<Workspace, String> {
    let updated: Workspace;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        ws.auto_port_forward = enabled;
        updated = ws.clone();
    }
    if !enabled {
        // Phase 47: turning detection off should ACTUALLY stop the
        // watcher (not just suppress events) and wipe what we've seen.
        clear_workspace_detection(&state, &app, &workspace_id);
        close_workspace_forwards(&state.core.forwards, &workspace_id);
    } else {
        // Phase 47: turning detection on while a session is already up
        // should start the watcher immediately. Best-effort: no-op if
        // no pane-backed SSH session has set up a tunnel yet.
        try_ensure_port_watcher(&state, &workspace_id).await;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

/// Phase 47: try to start the workspace's port-watcher. Best-effort —
/// returns silently (with a dlog) when no pane-backed SSH session has
/// set up a reverse tunnel yet (headless connect from Phase 41 doesn't
/// open one). spawn_ssh's own watcher launch will pick up later when a
/// terminal pane connects. Used by the activation effect, the toggle,
/// and the explicit `workspace_ensure_port_watcher` command.
async fn try_ensure_port_watcher(state: &AppState, workspace_id: &str) {
    let handle = match find_ssh_handle_for_workspace(state, workspace_id) {
        Some(h) => h,
        None => {
            dlog(&format!(
                "ensure_port_watcher[{workspace_id}]: no live SSH session — skip"
            ));
            return;
        }
    };
    let remote_port = {
        let m = state.core.internal_reverse_tunnel_remote_ports.lock().unwrap();
        m.get(workspace_id).and_then(|s| s.iter().next().copied())
    };
    let token = {
        let m = state.core.workspace_tunnel_tokens.lock().unwrap();
        m.get(workspace_id).cloned()
    };
    match (remote_port, token) {
        (Some(rp), Some(tok)) => {
            let _ = spawn_port_watcher(state, &handle, workspace_id, rp, &tok).await;
        }
        _ => {
            dlog(&format!(
                "ensure_port_watcher[{workspace_id}]: session has no reverse tunnel yet — open a terminal pane to bootstrap"
            ));
        }
    }
}

/// Phase 47: explicit command — frontend calls this on workspace
/// activation (when detection is on) to make sure a watcher is up.
/// Idempotent via spawn_port_watcher's dedup. Always Ok.
#[tauri::command]
async fn workspace_ensure_port_watcher(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    try_ensure_port_watcher(&state, &workspace_id).await;
    Ok(())
}

/// Phase 47: serializable shape for the snapshot endpoint.
#[derive(Clone, Serialize)]
pub(crate) struct DetectedPortInfo {
    pub remote_port: u16,
    pub addr: String,
    pub family: String,
}

/// Phase 47: snapshot the workspace's current detected_ports. Frontend
/// calls this on workspace switch to populate PortsWindow from state —
/// events alone aren't enough because they only fire while the FE was
/// already listening with the right workspace_id.
#[tauri::command]
async fn list_detected_ports(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<DetectedPortInfo>, String> {
    let m = state.core.detected_ports.lock().unwrap();
    let mut out: Vec<DetectedPortInfo> = m
        .get(&workspace_id)
        .map(|ports| {
            ports
                .iter()
                .map(|(port, (addr, family))| DetectedPortInfo {
                    remote_port: *port,
                    addr: addr.clone(),
                    family: family.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|d| d.remote_port);
    Ok(out)
}

// Phase 36 (#2.2): manually stop one forward (Ports panel "Stop
// forward" menu item).
#[tauri::command]
async fn port_forward_stop(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    remote_port: u16,
) -> Result<(), String> {
    close_one_forward(&state, &app, &workspace_id, remote_port);
    Ok(())
}

// Phase 46: open a forward on demand — driven by a user click on a
// detected port in PortsWindow. The watcher only detects now; this
// command is what actually opens the tunnel. Looks up the remote
// bind addr from `detected_ports` (falls back to "127.0.0.1" if
// missing) and hands off to `open_auto_forward`, which now runs a
// TCP sanity probe before reporting success. Idempotent — returns
// the existing local port if a forward already exists.
#[tauri::command]
async fn forward_port_start(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    remote_port: u16,
) -> Result<u16, String> {
    let addr = {
        let m = state.core.detected_ports.lock().unwrap();
        m.get(&workspace_id)
            .and_then(|ports| ports.get(&remote_port))
            .map(|(addr, _family)| addr.clone())
            .unwrap_or_else(|| "127.0.0.1".to_string())
    };
    open_auto_forward(&state, &app, &workspace_id, &addr, remote_port).await
}

// Phase 46: TCP sanity probe used by `open_auto_forward` to verify
// that a freshly-bound listener is actually reachable on 127.0.0.1
// before telling the FE the forward is live. Returns Ok if a
// connection succeeded within the timeout, Err with a reason
// otherwise. Pulled out as a free function so it's straightforward
// to unit-test against a known-good (just-bound) listener and a
// known-bad (vacant) port. Caller drops the returned stream — we
// only need to know that connect() succeeded.
#[cfg(test)]
pub(crate) async fn tcp_probe(
    target: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(target)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("connect failed: {e}")),
        Err(_) => Err(format!("timeout after {}ms", timeout.as_millis())),
    }
}

// Phase 31: per-pane identity. Same validation as the workspace
// command. Walks the workspace's layout to find the matching pane and
// updates its color/emoji fields. Returns a serializable snapshot of
// the pane after the update so the frontend can refresh its local
// state without waiting for the `workspaces:changed` round-trip.
#[derive(Clone, Serialize)]
pub(crate) struct PaneIdentity {
    pub(crate) pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) emoji: Option<String>,
}

fn set_pane_identity_in_layout(
    node: &mut LayoutNode,
    target: &str,
    new_color: &Option<String>,
    new_emoji: &Option<String>,
) -> Option<PaneIdentity> {
    match node {
        LayoutNode::Pane {
            pane_id,
            color,
            emoji,
            ..
        } if pane_id == target => {
            *color = new_color.clone();
            *emoji = new_emoji.clone();
            Some(PaneIdentity {
                pane_id: pane_id.clone(),
                color: color.clone(),
                emoji: emoji.clone(),
            })
        }
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            set_pane_identity_in_layout(first, target, new_color, new_emoji)
                .or_else(|| set_pane_identity_in_layout(second, target, new_color, new_emoji))
        }
    }
}

#[tauri::command]
async fn pane_set_identity(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    color: Option<String>,
    emoji: Option<String>,
) -> Result<PaneIdentity, String> {
    if let Some(c) = color.as_deref() {
        let bytes = c.as_bytes();
        let ok = bytes.len() == 7
            && bytes[0] == b'#'
            && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(format!("invalid color (want #rrggbb, got {c:?})"));
        }
    }
    if let Some(e) = emoji.as_deref() {
        if e.len() > 16 {
            return Err(format!("emoji too long ({} bytes, max 16)", e.len()));
        }
    }
    let updated: PaneIdentity;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_mut()
            .ok_or_else(|| format!("workspace {workspace_id} has no layout"))?;
        updated = set_pane_identity_in_layout(layout, &pane_id, &color, &emoji)
            .ok_or_else(|| format!("no pane {pane_id} in workspace {workspace_id}"))?;
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    Ok(updated)
}

#[tauri::command]
fn workspace_delete(
    state: State<'_, AppState>,
    app: AppHandle,
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
    // Phase 53 (rebased): drop the workspace-level Browser Webview
    // (at most one per workspace, keyed by workspace_id) and delete
    // the per-workspace browser-sessions directory (cookies /
    // localStorage / cache). Sessions DO survive transient hide/show
    // cycles; this is the only cleanup path that should wipe them.
    let webview = state
        .workspace_browsers
        .lock()
        .unwrap()
        .remove(&workspace_id);
    if let Some(w) = webview {
        let _ = w.close();
    }
    workspace_browser::cleanup_workspace_sessions(&workspace_id);
    for pane_id in &panes_to_kill {
        if let Some(sid) = state.core.pane_sessions.lock().unwrap().remove(pane_id) {
            if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
                kill_session_inner(&mut s);
            }
        }
    }
    // Phase 8.B: tear down any port forwards for the workspace.
    close_workspace_forwards(&state.core.forwards, &workspace_id);
    // Phase 39: drop the workspace's notes (the UI warns first when any
    // exist). Best-effort — failure here shouldn't block the delete.
    notes::delete_for_workspace(&state, &app, &workspace_id);
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
        file.active_workspace_id = workspace_id.clone();
        // Phase 49-C: stamp the activation timestamp on the workspace
        // being activated so the auto-destroy sweep can age it correctly.
        if let Some(id) = workspace_id.as_ref() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == *id) {
                ws.last_active_at = now;
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// Phase 49-B: anchor a workspace to a fresh git worktree.
//
// Runs `git worktree add <root>/<workspace_id>-<branch> -b <branch>
// <base>` from the workspace's cwd, then rewrites the workspace's cwd
// (and stamps `git_worktree`) so future panes spawn inside the worktree.
// Only valid for Local workspaces with an existing cwd that is itself
// a git repo. <root> defaults to `<config_dir>/worktrees`.
//
// Branch and base names are passed as standalone args to Command::new
// (no shell concatenation, per Absolute Rule #3). Branch name is also
// validated against an allow-list to keep it filesystem-safe.
#[tauri::command]
fn workspace_create_worktree(
    app: AppHandle,
    state: State<'_, AppState>,
    workspace_id: String,
    branch_name: String,
    base_branch: String,
) -> Result<WorkspacesFile, String> {
    // Sanitize the branch name for filesystem use. git itself allows
    // a wider set, but we own the directory naming so we constrain it.
    let safe_branch: String = branch_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/' { c } else { '-' })
        .collect();
    if safe_branch.is_empty() || safe_branch.starts_with('-') || safe_branch.contains("..") {
        return Err("invalid branch name".to_string());
    }
    if base_branch.trim().is_empty() {
        return Err("base branch is required".to_string());
    }
    // Snapshot the source cwd while holding the lock briefly.
    let src_cwd = {
        let file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| "workspace not found".to_string())?;
        match ws.connection {
            Some(Connection::Local { .. }) | None => {}
            _ => return Err("worktrees only apply to local workspaces".to_string()),
        }
        ws.cwd
            .clone()
            .ok_or_else(|| "workspace has no cwd to anchor a worktree to".to_string())?
    };
    let src_path = PathBuf::from(&src_cwd);
    if !src_path.join(".git").exists() {
        // .git can be a dir (regular repo) or file (submodule / worktree).
        return Err(format!("{src_cwd} is not a git repository"));
    }
    // Replace forward slashes in the branch with hyphens for the
    // directory name so feature/foo doesn't create nested dirs.
    let dir_branch = safe_branch.replace('/', "-");
    let root = config_dir()?.join("worktrees");
    std::fs::create_dir_all(&root).map_err(|e| format!("create worktrees root: {e}"))?;
    let target = root.join(format!("{workspace_id}-{dir_branch}"));
    if target.exists() {
        return Err(format!("target already exists: {}", target.display()));
    }
    let out = std::process::Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg(&target)
        .arg("-b")
        .arg(&branch_name)
        .arg(&base_branch)
        .current_dir(&src_path)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git worktree add failed: {}", stderr.trim()));
    }
    // Stamp the workspace and re-anchor its cwd to the new worktree.
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            ws.cwd = Some(target.to_string_lossy().into_owned());
            ws.git_worktree = Some(target.clone());
        }
    }
    persist(&state)?;
    dlog(&format!(
        "[worktree] created {} for ws={} branch={}",
        target.display(),
        workspace_id,
        branch_name,
    ));
    let _ = app.emit("workspaces:changed", ());
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
    // Phase 33: optional help-topic seed; used when pane_kind = Help.
    // None means "let split_pane_in pick the default topic".
    help_topic: Option<String>,
) -> Result<WorkspacesFile, String> {
    let kind = pane_kind.unwrap_or(PaneKind::Terminal);
    // Phase 23.C: when the new pane will be a Terminal, derive a
    // fallback connection BEFORE we mutate the layout. Three-tier
    // lookup:
    //   1. The source pane's own connection (handled inside split_pane_in).
    //   2. Any other terminal pane in this workspace.
    //   3. A live SSH session bound to this workspace (FileManager /
    //      Browser pane may be keeping it alive even when no terminal
    //      pane remains).
    // This fixes the bug where splitting from a FileManager/Browser
    // pane fell back to Local cmd instead of the workspace's SSH
    // connection.
    let fallback_conn: Option<Connection> = if matches!(kind, PaneKind::Terminal) {
        // Phase 23.D: four-tier fallback chain for the new pane's
        // connection — the workspace-level `connection` is now the
        // canonical truth, with the others as belt-and-suspenders
        // for older JSON / mid-session edge cases.
        //   1. first Terminal pane's connection in the layout
        //   2. workspace.connection (canonical)
        //   3. live SSH session bound to the workspace
        //   4. Local (only if all of the above are absent)
        let (layout_fallback, ws_conn) = {
            let file = state.workspaces.lock().unwrap();
            let ws = file.workspaces.iter().find(|w| w.id == workspace_id);
            (
                ws.and_then(|w| w.layout.as_ref().and_then(first_terminal_connection)),
                ws.and_then(|w| w.connection.clone()),
            )
        };
        layout_fallback
            .or(ws_conn)
            .or_else(|| live_ssh_connection_for_workspace(&state, &workspace_id))
    } else {
        None
    };
    {
        let mut file = state.workspaces.lock().unwrap();
        if let Some(ws) = file.workspaces.iter_mut().find(|w| w.id == workspace_id) {
            if let Some(layout) = ws.layout.take() {
                let (new_layout, _) = split_pane_in(
                    layout,
                    &pane_id,
                    direction,
                    kind,
                    browser_url,
                    fallback_conn,
                    help_topic,
                );
                ws.layout = Some(new_layout);
            }
        }
    }
    persist(&state)?;
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Phase 8.A: browser-pane commands ───────────────────────────────────────

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

#[tauri::command]
fn workspace_close_pane(
    state: State<'_, AppState>,
    workspace_id: String,
    pane_id: String,
) -> Result<WorkspacesFile, String> {
    let removed_pane: Option<String>;
    // Phase 23.B: capture whether the workspace still has any
    // SSH-consuming non-terminal panes AFTER the close. If yes, we
    // must keep the SSH handle alive even though the terminal pane
    // is gone — the file-manager / browser uses
    // `pick_ssh_handle_for_workspace` which scans the live sessions
    // for one matching the workspace_id. Killing the SSH session
    // here would leave those panes dead with no UI to reconnect.
    let keep_ssh_alive: bool;
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
        keep_ssh_alive = new_root
            .as_ref()
            .map(layout_has_ssh_consumer_pane)
            .unwrap_or(false);
        ws.layout = new_root;
        removed_pane = removed;
    }
    if let Some(pid) = removed_pane.as_ref() {
        // Phase 50: stop any diff-pane watcher bound to the removed
        // pane. Idempotent — no-op for non-Diff panes.
        diff_pane::stop_watcher(&state, pid);
    }
    if let Some(pid) = removed_pane {
        // Always unbind the pane from its session — the pane is gone.
        let sid_opt = state.core.pane_sessions.lock().unwrap().remove(&pid);
        if let Some(sid) = sid_opt {
            // Decide whether to actually drop the session. If the
            // session is SSH AND the workspace still has a consumer
            // (file-manager / browser pane), keep it alive so those
            // panes stay functional. Otherwise drop and clean up.
            let is_ssh_for_workspace = {
                let sessions = state.core.sessions.lock().unwrap();
                matches!(
                    sessions.get(&sid),
                    Some(Session::Ssh(ssh)) if ssh.workspace_id == workspace_id
                )
            };
            if is_ssh_for_workspace && keep_ssh_alive {
                tracing::info!(
                    "workspace_close_pane: keeping SSH session {sid} alive — workspace {workspace_id} still has FileManager/Browser pane(s)"
                );
                // Leave the session in state.core.sessions; it has no pane
                // binding now but `pick_ssh_handle_for_workspace`
                // will still find it via its workspace_id.
            } else if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
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

/// Phase 55-B: walk the workspace's layout tree and reset every
/// internal split's ratio to 0.5. Since the tree is binary, "1/N"
/// per the spec reduces to 0.5 per split — and that's also the
/// default new splits already get from split_pane_in, so a
/// distribute-evenly call effectively undoes every drag the user
/// has done on the dividers. Returns the updated WorkspacesFile so
/// the frontend can replace its local snapshot atomically.
fn reset_all_split_ratios(node: &mut LayoutNode) -> usize {
    match node {
        LayoutNode::Pane { .. } => 0,
        LayoutNode::Split {
            first,
            second,
            ratio,
            ..
        } => {
            *ratio = 0.5;
            1 + reset_all_split_ratios(first) + reset_all_split_ratios(second)
        }
    }
}

#[tauri::command]
fn workspace_distribute_evenly(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspacesFile, String> {
    let count;
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        count = ws
            .layout
            .as_mut()
            .map(reset_all_split_ratios)
            .unwrap_or(0);
    }
    persist(&state)?;
    let _ = app.emit("workspaces:changed", ());
    dlog(&format!(
        "workspace_distribute_evenly: ws={workspace_id} reset {count} split(s)"
    ));
    Ok(state.workspaces.lock().unwrap().clone())
}

// ─── Pane metadata (title / annotation) ─────────────────────────────────────

// Phase 52 (BiDi 33B): toggle the opt-in PTY-stream bidi filter on the
// given pane. Persists the bool onto the pane node (so the toggle
// survives reloads) AND updates the runtime filter map so the very
// next chunk is filtered (or not).
fn set_pane_smart_bidi_in_layout(node: &mut LayoutNode, target: &str, enabled: bool) -> bool {
    match node {
        LayoutNode::Pane {
            pane_id,
            smart_bidi,
            ..
        } if pane_id == target => {
            *smart_bidi = Some(enabled);
            true
        }
        LayoutNode::Pane { .. } => false,
        LayoutNode::Split { first, second, .. } => {
            set_pane_smart_bidi_in_layout(first, target, enabled)
                || set_pane_smart_bidi_in_layout(second, target, enabled)
        }
    }
}

#[tauri::command]
fn pane_set_smart_bidi(
    state: State<'_, AppState>,
    app: AppHandle,
    workspace_id: String,
    pane_id: String,
    enabled: bool,
) -> Result<WorkspacesFile, String> {
    {
        let mut file = state.workspaces.lock().unwrap();
        let ws = file
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
            .ok_or_else(|| format!("no workspace {workspace_id}"))?;
        let layout = ws
            .layout
            .as_mut()
            .ok_or_else(|| format!("workspace {workspace_id} has no layout"))?;
        if !set_pane_smart_bidi_in_layout(layout, &pane_id, enabled) {
            return Err(format!("no pane {pane_id} in workspace {workspace_id}"));
        }
    }
    persist(&state)?;
    // Flip the runtime filter for this pane right now so the next PTY
    // chunk takes the new state.
    bidi_filter::set_pane_enabled(&state.bidi_filters, &pane_id, enabled);
    let _ = app.emit("workspaces:changed", ());
    dlog(&format!(
        "[bidi] pane_set_smart_bidi: ws={} pane={} enabled={}",
        workspace_id, pane_id, enabled
    ));
    Ok(state.workspaces.lock().unwrap().clone())
}

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
                ws.layout = Some(update_pane_in(layout, &pane_id, Some(normalized.clone()), None));
            }
        }
    }
    persist(&state)?;

    // Phase 23.K: if the pane has a live tmux session, update the
    // LOCAL label for it. Pure disk write (no SSH, no spawned task)
    // — sidesteps the Phase 23.I crash entirely. The picker reads
    // this map back in `tmux_labels_get` and shows the label as the
    // primary line, with the raw tmux session name as secondary.
    //
    // The Phase 23.J disabled remote-tmux-rename side-effect stays
    // disabled — labels give us the user-friendly Hebrew title
    // experience without crossing the FFI panic boundary.
    if let Some(label_text) = normalized.as_deref() {
        let tmux_target = {
            let pane_sessions = state.core.pane_sessions.lock().ok();
            let sid = pane_sessions
                .as_ref()
                .and_then(|m| m.get(&pane_id).cloned());
            drop(pane_sessions);
            sid.and_then(|sid| {
                state.core.sessions.lock().ok().and_then(|sessions| {
                    match sessions.get(&sid) {
                        Some(Session::Ssh(s)) => s.tmux_session.clone(),
                        _ => None,
                    }
                })
            })
        };
        if let Some(tmux_name) = tmux_target {
            set_tmux_label_internal(&workspace_id, &tmux_name, label_text);
        }
    }

    let _ = app.emit("workspaces:changed", ());
    Ok(state.workspaces.lock().unwrap().clone())
}

/// Phase 23.I helper: look up the SSH session bound to a pane and
/// return (session_id, ssh handle clone, current tmux session name).
/// Returns None if the pane has no session, the session is not SSH,
/// or it has no tmux wrapper.
/// Phase 23.J: orphaned for now — the only caller (the spawned
/// rename task in pane_set_title) was disabled pending root-cause
/// of the Hebrew-title crash. Kept in place so Phase 23.K can
/// re-enable without re-writing it.
#[allow(dead_code)]
fn lookup_tmux_for_pane(
    state: &AppState,
    pane_id: &str,
) -> Option<(String, Arc<client::Handle<SshClient>>, String)> {
    let pane_sessions = state.core.pane_sessions.lock().ok()?;
    let sid = pane_sessions.get(pane_id)?.clone();
    drop(pane_sessions);
    let sessions = state.core.sessions.lock().ok()?;
    match sessions.get(&sid) {
        Some(Session::Ssh(s)) => s
            .tmux_session
            .as_ref()
            .map(|t| (sid.clone(), s.handle.clone(), t.clone())),
        _ => None,
    }
}

/// Phase 23.I helper: run `tmux rename-session -t <old> <new>` over an
/// existing SSH handle. Shared by pane_set_title and the legacy 23.G
/// tmux_rename_session tauri command. Validates names defensively
/// (no spaces/dots/colons) — `sanitize_tmux_session_name_for_title`
/// already collapses those, but a direct CLI caller might not.
async fn tmux_rename_session_via_handle(
    handle: &client::Handle<SshClient>,
    old_name: &str,
    new_name: &str,
) -> Result<(), String> {
    if new_name.is_empty() {
        return Err("name cannot be empty".into());
    }
    if new_name.chars().any(|c| c == '.' || c == ':') {
        return Err("name cannot contain dots or colons".into());
    }
    let cmd = format!(
        "tmux rename-session -t '{}' '{}' 2>&1",
        old_name.replace('\'', "'\\''"),
        new_name.replace('\'', "'\\''"),
    );
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let mut exit_code: Option<u32> = None;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    let stderr_text = String::from_utf8_lossy(&stdout).trim().to_string();
    match exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(if stderr_text.is_empty() {
            format!("tmux exit {code}")
        } else {
            stderr_text
        }),
        None => Err("tmux rename-session did not return an exit status".into()),
    }
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

/// Phase 41: establish a background ("headless") SSH session for a
/// workspace without opening a pane, so the tmux session picker and the
/// remote file manager populate immediately on workspace select.
///
/// Idempotent — a no-op if any SSH session (headless or pane-backed)
/// already serves the workspace. Only agent/key auth is attempted
/// (`password: None`); password-mode workspaces are skipped silently with
/// a dlog (no UI to prompt from here — they connect when the user opens a
/// terminal pane). An unknown host key also skips silently rather than
/// auto-accepting in the background.
#[tauri::command]
async fn workspace_ensure_connected(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    // Fast idempotency check before doing any network work.
    if live_ssh_connection_for_workspace_pub(&state, &workspace_id).is_some() {
        return Ok(());
    }

    // Resolve the workspace's canonical SSH target.
    let conn = {
        let file = state.workspaces.lock().unwrap();
        file.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.connection.clone())
    };
    let (host, user, port, key_path) = match conn {
        Some(Connection::Ssh {
            host,
            user,
            port,
            key_path,
        }) => (host, user, port, key_path),
        // Local workspace or no connection — nothing to auto-connect.
        _ => return Ok(()),
    };

    // agent/key only; never auto-accept an unknown host key in the background.
    match connect_and_authenticate(&host, &user, port, key_path.as_deref(), None, None, false).await
    {
        // Phase 47.A: capture tunnel_token (Phase 41 dropped it) and
        // keep `handle` mutable — `tcpip_forward` inside
        // `setup_workspace_reverse_tunnel` needs &mut, so the tunnel
        // setup must happen BEFORE the handle is moved into Arc and
        // stored in the session.
        Ok(SshHandshake {
            mut handle,
            auth_method,
            tunnel_token,
        }) => {
            // Quick idempotency pre-check (a pane may have already
            // connected). If so, drop the spare handle now.
            {
                let sessions = state.core.sessions.lock().unwrap();
                let already = sessions
                    .values()
                    .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_id));
                if already {
                    dlog(&format!(
                        "workspace_ensure_connected: {workspace_id} connected by a pane mid-auth — dropping spare headless handle"
                    ));
                    return Ok(());
                }
            }
            // Phase 47.A: bootstrap the reverse tunnel before Arc-wrapping
            // so port detection works without a terminal pane. Best-effort:
            // failure leaves the session usable for tmux-list / file
            // manager, just no detection (matches pre-47.A behavior).
            let _ = setup_workspace_reverse_tunnel(
                &state,
                &mut handle,
                &workspace_id,
                &tunnel_token,
            )
            .await;
            // Re-check + insert under the lock. If a pane raced in during
            // tunnel setup, drop the spare (its handle Drop tears the
            // tunnel down with it).
            let mut sessions = state.core.sessions.lock().unwrap();
            let already = sessions
                .values()
                .any(|s| matches!(s, Session::Ssh(ssh) if ssh.workspace_id == workspace_id));
            if already {
                dlog(&format!(
                    "workspace_ensure_connected: {workspace_id} connected by a pane mid-tunnel-setup — dropping spare headless handle"
                ));
                return Ok(());
            }
            sessions.insert(
                format!("__headless__{workspace_id}"),
                Session::Ssh(SshSession {
                    tx: None,
                    handle: Arc::new(handle),
                    workspace_id: workspace_id.clone(),
                    tmux_session: None,
                    host,
                    user,
                    port,
                    key_path,
                }),
            );
            drop(sessions);
            dlog(&format!(
                "workspace_ensure_connected: headless session up for {workspace_id} (method={auth_method:?})"
            ));
            Ok(())
        }
        Err(e) => {
            // Most commonly: no key/agent → password-only, which we can't
            // prompt for here. Skip silently; the terminal-pane path handles it.
            dlog(&format!(
                "workspace_ensure_connected: skipped for {workspace_id}: {e}"
            ));
            Ok(())
        }
    }
}

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
    // Phase 23.F: when set AND we're in a persistent flow, override
    // the auto-derived tmux session name. Lets the user attach to a
    // previously-orphaned session whose original pane was closed.
    tmux_session_name: Option<String>,
) -> Result<String, String> {
    // Look up connection from workspaces state. Phase 7.C: also lift `env` and
    // `setup_command` from the workspace so we can inject them after the shell is up.
    // Phase 23.I: also lift the pane's title so the persistent (tmux) flow can
    // derive a session name from it instead of the opaque pane-id default.
    let (conn, cwd, ws_env, ws_setup, pane_title) = {
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
        // Phase 23.D: prefer the pane's own connection, but fall
        // back to the workspace-level `connection` when the pane
        // doesn't carry one. This lets the user reconnect to the
        // workspace's intended target from a fresh terminal pane
        // (e.g. one added via split off a FileManager/Browser)
        // even if pane.connection was never set, AND enforces "an
        // SSH workspace never accidentally spawns a local shell"
        // semantics requested by Yossi.
        let conn = find_pane_connection(layout, &pane_id)
            .or_else(|| {
                if pane_id_exists_in(layout, &pane_id) {
                    ws.connection.clone()
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                if pane_id_exists_in(layout, &pane_id) {
                    format!("pane {pane_id} is not a terminal pane and workspace has no connection")
                } else {
                    format!("no pane {pane_id}")
                }
            })?;
        let title = find_pane_title(layout, &pane_id);
        (
            conn,
            ws.cwd.clone(),
            ws.env.clone(),
            ws.setup_command.clone(),
            title,
        )
    };

    // Phase 23.I: resolve the effective tmux session name BEFORE we
    // hand off to spawn_ssh. Precedence:
    //   1. Caller-supplied tmux_session_name (picker chose explicit
    //      existing-session attach)
    //   2. Sanitized pane title (pane title IS the tmux session name —
    //      Hebrew/Arabic/CJK titles supported)
    //   3. None — spawn_ssh's tmux_name derivation falls back to
    //      `sanitize_tmux_session_name(&pane_id)` (the legacy
    //      "winmux-<paneid>" auto-name)
    let effective_tmux_name: Option<String> = tmux_session_name
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            pane_title
                .as_deref()
                .and_then(sanitize_tmux_session_name_for_title)
        });

    // Resolve shell kind for env-line formatting (need this BEFORE we move `conn`).
    let shell_kind = match &conn {
        Connection::Local { shell } => detect_shell_kind(&pick_default_shell(shell.clone())),
        Connection::Ssh { .. } => ShellKind::Posix,
    };

    // Kill any prior session for this pane.
    if let Some(old_sid) = state.core.pane_sessions.lock().unwrap().remove(&pane_id) {
        if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&old_sid) {
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
                effective_tmux_name.clone(),
            )
            .await?
        }
    };
    state.core
        .pane_sessions
        .lock()
        .unwrap()
        .insert(pane_id, session_id.clone());

    // Phase 7.C: inject env exports + setup_command after a 500ms grace period.
    schedule_setup_injection(
        state.core.sessions.clone(),
        session_id.clone(),
        shell_kind,
        ws_env,
        ws_setup,
    );

    // Phase 12.B Smart Connect: when mode is "cmd" or "claude", inject the
    // command after a 1.1s delay (after env exports + setup_command + tmux
    // wrap have all settled). cwd_override changes directory first.
    // Phase 61: the script is shaped per shell_kind, so local PowerShell /
    // Cmd panes get working syntax too (POSIX `exec` form unchanged).
    let smart_mode = mode.clone();
    if matches!(smart_mode.as_deref(), Some("cmd") | Some("claude")) {
        let script = build_smart_connect_script(
            shell_kind,
            smart_mode.as_deref().unwrap_or_default(),
            cwd_override.as_deref(),
            cmd.as_deref(),
            claude_args.as_deref(),
        );
        if !script.is_empty() {
            let sessions_clone = state.core.sessions.clone();
            let session_id_clone = session_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                let mut sessions = sessions_clone.lock().unwrap();
                if let Some(s) = sessions.get_mut(&session_id_clone) {
                    match s {
                        Session::Local(l) => {
                            use std::io::Write as _;
                            let _ = l.writer.write_all(script.as_bytes());
                            let _ = l.writer.flush();
                        }
                        Session::Ssh(ssh) => {
                            let _ = ssh.try_send(SshCmd::Data(script.into_bytes()));
                        }
                    }
                }
            });
        }
    }

    Ok(session_id)
}

/// Phase 23.F: tmux session metadata returned by
/// pane_list_tmux_sessions for the Connect (tmux) picker modal.
#[derive(Clone, Serialize)]
pub(crate) struct TmuxSessionInfo {
    pub name: String,
    pub created: i64,
    pub attached: bool,
    pub windows: u32,
    pub last_attached: i64,
}

/// Phase 23.F: enumerate the tmux sessions live on a workspace's
/// host. Returns Ok([]) when tmux isn't installed or no sessions
/// exist. Used by the Connect (tmux) split-button to populate a
/// picker so users can attach to an orphan session whose original
/// pane was closed.
#[tauri::command]
async fn pane_list_tmux_sessions(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<TmuxSessionInfo>, String> {
    // Phase 23.H: silent Ok([]) fallback when no live SSH handle.
    // Previously we errored ("no active SSH session for this workspace"),
    // but the user typically clicks Connect (tmux) BEFORE any pane has
    // authenticated — the whole point is to pick an orphan session before
    // connecting. Returning Ok([]) lets the picker render its "New session"
    // option + the "No existing sessions" empty-state line, which is
    // accurate ("no sessions visible from winmux right now") and avoids
    // surfacing a red error for the most common access pattern. Once a
    // terminal pane authenticates, re-opening the picker will list the
    // real sessions over the now-live handle.
    let handle = {
        let sessions = state.core.sessions.lock().unwrap();
        sessions
            .iter()
            .find_map(|(_sid, sess)| match sess {
                Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
                _ => None,
            })
    };
    let handle = match handle {
        Some(h) => h,
        None => {
            dlog(&format!(
                "pane_list_tmux_sessions: no live SSH handle for ws={workspace_id}, returning empty list"
            ));
            return Ok(vec![]);
        }
    };
    let script = "tmux list-sessions -F '#{session_name}|#{session_created}|#{session_attached}|#{session_windows}|#{session_last_attached}' 2>/dev/null || true";
    use russh::ChannelMsg;
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, script.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
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
    let text = String::from_utf8_lossy(&stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 5 { continue; }
        out.push(TmuxSessionInfo {
            name: parts[0].to_string(),
            created: parts[1].parse().unwrap_or(0),
            attached: parts[2] == "1",
            windows: parts[3].parse().unwrap_or(0),
            last_attached: parts[4].parse().unwrap_or(0),
        });
    }
    out.sort_by(|a, b| b.last_attached.max(b.created).cmp(&a.last_attached.max(a.created)));
    Ok(out)
}

// ─── Phase 23.K: local tmux session labels ─────────────────────────────────
//
// User-friendly Hebrew/Arabic/CJK label for each tmux session, stored
// locally on the Windows host (NOT in tmux itself). The Phase 23.I
// experiment of actually renaming the remote tmux session crashed on
// Hebrew (see Phase 23.J root-cause notes). Labels sidestep that
// entirely: tmux session names stay ASCII / safe, but the picker UI
// shows whatever the user typed in the pane title.
//
// File: %APPDATA%/winmux/tmux-labels.json
// Schema: { version: 1, labels: { workspace_id: { session_name: label } } }

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct TmuxLabelsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub labels: HashMap<String, HashMap<String, String>>,
}

fn tmux_labels_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("tmux-labels.json"))
}

fn load_tmux_labels() -> TmuxLabelsFile {
    let path = match tmux_labels_path() {
        Ok(p) => p,
        Err(_) => return TmuxLabelsFile::default(),
    };
    if !path.exists() {
        return TmuxLabelsFile::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            dlog(&format!("tmux-labels: read failed: {e}"));
            return TmuxLabelsFile::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        dlog(&format!("tmux-labels: parse failed: {e}"));
        TmuxLabelsFile::default()
    })
}

fn save_tmux_labels(file: &TmuxLabelsFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = tmux_labels_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("tmux-labels.{}.tmp", std::process::id()));
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
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Internal helper used by both the tauri command and pane_set_title's
/// auto-label hook. Empty label clears the entry; clearing the last
/// entry in a workspace also removes the workspace key for cleanliness.
fn set_tmux_label_internal(workspace_id: &str, session_name: &str, label: &str) {
    let mut file = load_tmux_labels();
    let trimmed = label.trim();
    if trimmed.is_empty() {
        if let Some(ws_map) = file.labels.get_mut(workspace_id) {
            ws_map.remove(session_name);
            if ws_map.is_empty() {
                file.labels.remove(workspace_id);
            }
        }
    } else {
        file.labels
            .entry(workspace_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(session_name.to_string(), trimmed.to_string());
    }
    if let Err(e) = save_tmux_labels(&file) {
        dlog(&format!("tmux-labels: save failed: {e}"));
    }
}

#[tauri::command]
fn tmux_labels_get(workspace_id: String) -> HashMap<String, String> {
    let file = load_tmux_labels();
    file.labels.get(&workspace_id).cloned().unwrap_or_default()
}

#[tauri::command]
fn tmux_label_set(
    workspace_id: String,
    session_name: String,
    label: Option<String>,
) -> Result<(), String> {
    if session_name.is_empty() {
        return Err("session_name cannot be empty".into());
    }
    set_tmux_label_internal(&workspace_id, &session_name, label.as_deref().unwrap_or(""));
    Ok(())
}

/// Phase 23.G: rename a tmux session over the workspace's SSH
/// handle. The Phase 23.G in-picker Rename button was removed in
/// Phase 23.I (pane title became the canonical session name) — this
/// command stays registered for any future CLI / programmatic caller
/// (e.g. Phase 24 bulk renames). Now delegates to the shared
/// `tmux_rename_session_via_handle` helper that pane_set_title uses.
#[tauri::command]
async fn tmux_rename_session(
    state: State<'_, AppState>,
    workspace_id: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    if old_name.is_empty() {
        return Err("old_name cannot be empty".into());
    }
    let handle = {
        let sessions = state.core.sessions.lock().unwrap();
        sessions
            .iter()
            .find_map(|(_sid, sess)| match sess {
                Session::Ssh(s) if s.workspace_id == workspace_id => Some(s.handle.clone()),
                _ => None,
            })
    }
    .ok_or_else(|| "no active SSH session for this workspace".to_string())?;
    tmux_rename_session_via_handle(&handle, &old_name, &new_name).await
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
        let sessions = state.core.sessions.lock().unwrap();
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
    let sessions_map = state.core.pane_sessions.lock().unwrap();
    let sid = sessions_map.get(&pane_id)?.clone();
    drop(sessions_map);
    let sessions = state.core.sessions.lock().unwrap();
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
    let pane_sessions = state.core.pane_sessions.lock().unwrap().clone();
    let sessions = state.core.sessions.lock().unwrap();
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
    let sid_opt = state.core.pane_sessions.lock().unwrap().get(&pane_id).cloned();
    let Some(sid) = sid_opt else {
        return Ok(());
    };
    // Snapshot the SSH handle + tmux name without holding the lock across the
    // .await — russh's Handle is shared as Arc<> so this is cheap.
    let (handle_arc, tmux_name) = {
        let sessions = state.core.sessions.lock().unwrap();
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
    let sid = state.core.pane_sessions.lock().unwrap().remove(&pane_id);
    if let Some(sid) = sid {
        if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
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
    let sid = state.core.pane_sessions.lock().unwrap().remove(&pane_id);
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
            let mut sessions = state.core.sessions.lock().unwrap();
            if let Some(s) = sessions.get_mut(&sid) {
                match s {
                    Session::Local(l) => {
                        use std::io::Write as _;
                        let _ = l.writer.write_all(&bytes);
                        let _ = l.writer.flush();
                    }
                    Session::Ssh(ssh) => {
                        let _ = ssh.try_send(SshCmd::Data(bytes));
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if let Some(mut s) = state.core.sessions.lock().unwrap().remove(&sid) {
        kill_session_inner(&mut s);
    }
    Ok(())
}

// ─── Session-level commands (write/resize) ───────────────────────────────────

pub(crate) fn write_to_session(state: &AppState, session_id: &str, data: &[u8]) -> Result<(), String> {
    let mut sessions = state.core.sessions.lock().unwrap();
    let s = sessions
        .get_mut(session_id)
        .ok_or_else(|| format!("no such session {session_id}"))?;
    match s {
        Session::Local(l) => {
            l.writer.write_all(data).map_err(|e| e.to_string())?;
            l.writer.flush().map_err(|e| e.to_string())?;
        }
        Session::Ssh(ssh) => {
            ssh.try_send(SshCmd::Data(data.to_vec()))?;
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

// Phase 48-C: build the /doctor diagnostic snapshot. Process-cheap
// signals only — no shell-outs, no FS scans beyond a small log tail.
// Reused by the `doctor` tauri command, the `doctor` RPC method, and
// the `winmux doctor` CLI subcommand.
pub(crate) fn build_doctor_snapshot(state: &AppState) -> serde_json::Value {
    use std::sync::atomic::Ordering;
    let workspaces = state.workspaces.lock().unwrap().workspaces.clone();
    let workspace_count = workspaces.len();
    // Count which workspaces have a live SSH session (any pane or the
    // headless Phase 41 entry counts).
    let sessions = state.core.sessions.lock().unwrap();
    let mut ssh_connected = std::collections::HashSet::new();
    let mut pty_count = 0usize;
    for s in sessions.values() {
        pty_count += 1;
        if let Session::Ssh(ssh) = s {
            ssh_connected.insert(ssh.workspace_id.clone());
        }
    }
    let ssh_connected_count = ssh_connected.len();
    drop(sessions);

    let bundled_cli_sha256: Option<String> = (|| {
        // v0.2.7 scrub fix: embed the manifest content at COMPILE time
        // via include_str! rather than read it at runtime from a path
        // that only exists on the build machine. The previous
        // env!("CARGO_MANIFEST_DIR") + fs::read_to_string approach
        // leaked the developer's absolute build path into the release
        // binary (RUSTFLAGS --remap-path-prefix only scrubs debug
        // info, not env!() string expansions) AND silently failed at
        // runtime on every user machine where that path didn't exist.
        const MANIFEST: &str = include_str!("../resources/remote-manifest.json");
        let m: serde_json::Value = serde_json::from_str(MANIFEST.trim_start_matches('\u{FEFF}'))
            .ok()?;
        m.get("x86_64-linux")?
            .get("sha256")?
            .as_str()
            .map(|s| s.to_string())
    })();

    // Last few lines from debug.log filtered to ERROR/WARN. Best-effort.
    let recent_errors: Vec<String> = (|| -> Option<Vec<String>> {
        let path = config_dir_pub().ok()?.join("debug.log");
        let s = std::fs::read_to_string(&path).ok()?;
        let mut out: Vec<String> = s
            .lines()
            .rev()
            .filter(|l| l.contains("ERROR") || l.contains("WARN"))
            .take(10)
            .map(|s| s.to_string())
            .collect();
        out.reverse();
        Some(out)
    })()
    .unwrap_or_default();

    serde_json::json!({
        "winmux_version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "workspaces": {
            "total": workspace_count,
            "ssh_connected": ssh_connected_count,
        },
        "pty_sessions": pty_count,
        "rpc_server": {
            "pipe_name": rpc_server::pipe_name(),
            "listener_pool_size": 8,
            "handlers_served": rpc_server::HANDLER_SEQ.load(Ordering::Relaxed),
        },
        "bundled_linux_cli_sha256": bundled_cli_sha256,
        "recent_errors": recent_errors,
    })
}

#[tauri::command]
fn doctor(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    Ok(build_doctor_snapshot(&state))
}

// Phase 48-D: frontend stall instrumentation. The FE drives a 100ms
// heartbeat and a longtask PerformanceObserver; when either spots
// >threshold gaps, it calls this to record them in debug.log with a
// `[ui]` prefix so post-hoc support tickets can correlate UI jank
// with backend activity.
#[tauri::command]
fn diag_log(level: String, msg: String) -> Result<(), String> {
    let lvl = match level.to_ascii_lowercase().as_str() {
        "error" => "ERROR",
        "warn" | "warning" => "WARN",
        _ => "INFO",
    };
    dlog(&format!("[ui] {lvl}: {msg}"));
    Ok(())
}

#[tauri::command]
fn pty_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state.core.sessions.lock().unwrap();
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
        Session::Ssh(ssh) => ssh.try_send(SshCmd::Resize(cols as u32, rows as u32)),
    }
}

// ─── Entrypoint ──────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok();

    // Phase 23.J: capture every panic to debug.log so the next
    // reproduction tells us EXACTLY what panicked, instead of dying
    // silently to WinDbg with no info. The Phase 23.I Hebrew-title
    // crash was a STATUS_STACK_BUFFER_OVERRUN (__fastfail(7)) with
    // no Rust panic trace anywhere — we had to reverse-engineer the
    // cause from WER event metadata and 5-second timing. This hook
    // eliminates that guesswork for next time.
    //
    // RUST_BACKTRACE=1 is set unconditionally before the hook so
    // `Backtrace::capture()` always returns frames (otherwise the
    // env var defaults to off and capture() returns "disabled").
    // Safe to set in dev builds; revisit for release.
    std::env::set_var("RUST_BACKTRACE", "1");
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let bt = std::backtrace::Backtrace::capture();
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();
        dlog(&format!(
            "PANIC at {location}: {msg}\n  thread: {thread_name}\n  backtrace:\n{bt}"
        ));
        // Re-emit to stderr so any wrapping process (cargo run, tauri
        // dev server, etc.) can also surface it inline.
        eprintln!("PANIC at {location}: {msg}");
    }));

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

            // Phase 53.G: was Phase 8.F.1 — the iframe-bridge
            // initialization script was the parent-side companion to
            // the per-pane iframe Browser. With the per-pane Browser
            // surface gone (53.D moved Browser to a workspace-level
            // child Webview via Window::add_child) the bridge is
            // dead. The main window is still created programmatically
            // because tauri.conf.json's `windows: []` skips the
            // default — same title + inner size as before.
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("winmux")
            .inner_size(1100.0, 700.0)
            .build()
            .map_err(|e| Box::<dyn std::error::Error>::from(format!("main window: {e}")))?;
            dlog("setup: main webview created");
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
            // Phase 12.C: load recent paths history (or empty on first run).
            match local_wizard::load_recent_from_disk() {
                Ok(rf) => {
                    let count = rf.entries.len();
                    *state.recent_paths.lock().unwrap() = rf;
                    dlog(&format!("setup: recent_paths loaded ({count} entries)"));
                }
                Err(e) => {
                    dlog(&format!("setup: recent_paths load failed: {e} (starting empty)"));
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
            // Phase 39.B: one-time migration. Workspaces created before
            // Phase 39 flipped the auto_port_forward default still have
            // `true` saved and keep auto-forwarding on every connect
            // (the WINMUX-CHALLENGE / pipe-storm path). Flip them to
            // false once; users re-enable per workspace. The flag on
            // Settings keeps this from re-running and undoing a later
            // opt-in. Skipped if workspaces failed to load (load_state
            // != Loaded) so we never persist over a clobbered file.
            {
                let load_ok =
                    *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let already_done = state
                    .settings
                    .lock()
                    .unwrap()
                    .migrations
                    .phase_39_auto_port_forward_default_flipped;
                if load_ok && !already_done {
                    let changed = {
                        let mut f = state.workspaces.lock().unwrap();
                        disable_all_auto_port_forward(&mut f)
                    };
                    if changed > 0 {
                        match persist(&state) {
                            Ok(()) => dlog(&format!(
                                "migration phase_39: flipped {changed} workspace(s) auto_port_forward to false"
                            )),
                            Err(e) => dlog(&format!("migration phase_39: save failed: {e}")),
                        }
                    } else {
                        dlog("migration phase_39: no workspaces needed flipping");
                    }
                    // Mark done + persist settings (do this regardless of
                    // `changed` so the migration never re-runs).
                    let snapshot = {
                        let mut s = state.settings.lock().unwrap();
                        s.migrations.phase_39_auto_port_forward_default_flipped = true;
                        s.clone()
                    };
                    if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                        dlog(&format!("migration phase_39: settings save failed: {e}"));
                    }
                }
            }
            // Phase 53 (rebased): one-time rewrite of any leftover
            // PaneKind::Browser / PaneKind::FileManager panes to
            // Terminal. These pane kinds were removed from the
            // create-pane menu when the Browser + Files surfaces moved
            // to workspace-level floating windows; the leftover panes
            // would render as broken under the new layout. Same
            // safety gate as the phase_39 migration: skip if
            // load_state != Loaded.
            {
                let load_ok =
                    *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let already_done = state
                    .settings
                    .lock()
                    .unwrap()
                    .migrations
                    .phase_53_remove_browser_filemanager_panes;
                if load_ok && !already_done {
                    let changed = {
                        let mut f = state.workspaces.lock().unwrap();
                        rewrite_browser_filemanager_panes_to_terminal(&mut f)
                    };
                    if changed > 0 {
                        match persist(&state) {
                            Ok(()) => dlog(&format!(
                                "migration phase_53: rewrote {changed} Browser/FileManager pane(s) to Terminal"
                            )),
                            Err(e) => dlog(&format!("migration phase_53: save failed: {e}")),
                        }
                    } else {
                        dlog("migration phase_53: no legacy Browser/FileManager panes found");
                    }
                    let snapshot = {
                        let mut s = state.settings.lock().unwrap();
                        s.migrations.phase_53_remove_browser_filemanager_panes = true;
                        s.clone()
                    };
                    if let Err(e) = settings::save_to_disk_pub(&snapshot) {
                        dlog(&format!("migration phase_53: settings save failed: {e}"));
                    }
                }
            }
            // Phase 49-C: auto-destroy sweep. Opt-in via
            // settings.auto_destroy_empty_workspaces_days. A workspace is
            // a candidate when it has no panes (empty layout) AND its
            // last_active_at is older than the configured TTL. Sessions
            // aren't checked — startup runs BEFORE any spawn_ssh, so
            // there's nothing live yet. last_active_at = 0 (never
            // activated since the field was added) is grace-treated as
            // "recent" so the first run after upgrade doesn't nuke
            // never-touched workspaces. Silent — the user opted in via
            // the setting; no toast.
            {
                let load_ok = *state.load_state.lock().unwrap() == Some(LoadState::Loaded);
                let ttl_days = state
                    .settings
                    .lock()
                    .unwrap()
                    .auto_destroy_empty_workspaces_days;
                if load_ok {
                    if let Some(days) = ttl_days {
                        if days > 0 {
                            let ttl_secs = (days as u64) * 86_400;
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let removed = {
                                let mut f = state.workspaces.lock().unwrap();
                                let before = f.workspaces.len();
                                f.workspaces.retain(|w| {
                                    let stale = w.last_active_at > 0
                                        && now.saturating_sub(w.last_active_at) > ttl_secs;
                                    let empty = w.layout.is_none();
                                    if stale && empty {
                                        dlog(&format!(
                                            "auto-destroy: removing workspace {} ({}) — empty + last_active {} days ago",
                                            w.id,
                                            w.name,
                                            now.saturating_sub(w.last_active_at) / 86_400
                                        ));
                                        false
                                    } else {
                                        true
                                    }
                                });
                                before - f.workspaces.len()
                            };
                            if removed > 0 {
                                if let Err(e) = persist(&state) {
                                    dlog(&format!("auto-destroy: save failed: {e}"));
                                }
                            }
                        }
                    }
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
            workspace_set_identity,
            workspace_set_auto_port_forward,
            port_forward_stop,
            forward_port_start,
            workspace_ensure_port_watcher,
            list_detected_ports,
            log_dir_path,
            read_log_tail,
            pane_set_identity,
            pane_set_smart_bidi,
            workspace_browser::workspace_browser_show,
            workspace_browser::workspace_browser_hide,
            workspace_browser::workspace_browser_navigate,
            workspace_browser::workspace_browser_eval,
            workspace_browser::workspace_browser_close,
            workspace_browser::workspace_browser_resize,
            ssh_key_offer_dismiss,
            ssh_key_generate_and_install,
            provision_existing_install_key,
            workspace_delete,
            workspace_set_active,
            workspace_create_worktree,
            workspace_split,
            workspace_close_pane,
            workspace_set_split_ratio,
            workspace_distribute_evenly,
            workspace_ensure_connected,
            pane_connect,
            pane_disconnect,
            pane_kill_session,
            pane_persistence_get,
            pane_persistence_list,
            pane_list_claude_sessions,
            pane_list_tmux_sessions,
            tmux_rename_session,
            tmux_labels_get,
            tmux_label_set,
            pane_set_title,
            pane_set_annotation,
            workspace_reset_layout,
            dev_console_log,
            pty_write,
            pty_resize,
            diag_log,
            doctor,
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
            updater::download_and_install_update,
            updater::ssh_exec_in_workspace,
            connect_wizard::parse_ssh_config,
            connect_wizard::list_ssh_keys,
            connect_wizard::check_key_permissions,
            connect_wizard::fix_key_permissions,
            connect_wizard::test_ssh_connect,
            provisioning::provisioning_inspect,
            provisioning::provisioning_start,
            provisioning::connect_existing_discover,
            provisioning::provisioning_profiles_list,
            provisioning::provisioning_profile_save,
            provisioning::provisioning_profile_delete,
            provisioning::provisioning_step_catalog,
            file_manager::file_list_local,
            file_manager::file_list_remote,
            file_manager::file_home_local,
            file_manager::file_home_remote,
            file_manager::file_delete_local,
            file_manager::file_delete_remote,
            file_manager::file_rename_local,
            file_manager::file_rename_remote,
            file_manager::file_mkdir_local,
            file_manager::file_mkdir_remote,
            file_manager::file_create_local,
            file_manager::file_create_remote,
            file_manager::file_upload,
            file_manager::file_upload_bytes,
            file_manager::pane_upload_dropped,
            diff_pane::diff_pane_set_source,
            diff_pane::diff_pane_refresh,
            file_manager::file_download,
            file_manager::download_remote_file_via_osc,
            file_manager::file_open_local,
            file_manager::file_open_remote,
            file_manager::file_read_local,
            file_manager::file_read_remote,
            file_manager::file_write_local,
            file_manager::file_write_remote,
            file_manager::file_large_threshold,
            file_manager::file_manager_zip_local,
            file_manager::file_manager_unzip_local,
            file_manager::file_manager_zip_remote,
            file_manager::file_manager_unzip_remote,
            file_manager::file_manager_unzip_local_check,
            file_manager::file_manager_unzip_remote_check,
            stt::stt_transcribe_local,
            claude_summary::claude_summarize,
            // Phase 24.D: claude_log_* commands KEPT (registered but
            // no FE caller) for a future unified-view rebuild.
            // claude_log_pane_set was removed alongside the pane kind.
            // claude_chat_* commands deleted with the module.
            claude_log::claude_log_sync,
            claude_log::claude_log_list,
            claude_log::claude_log_read,
            local_wizard::detect_local_shells,
            local_wizard::list_recent_paths,
            local_wizard::record_recent_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod port_forward_tests {
    // Phase 36 (#2.2) → 36.A: the forwards map is keyed by
    // (workspace_id, remote_port); local_port is now whatever the
    // kernel assigned at bind time (no longer derived from remote_port).
    // These exercise the insert / lookup / remove mechanics that
    // open_auto_forward + close_one_forward rely on, without a live
    // russh channel (cancel = None). The local_port values below stand
    // in for arbitrary kernel-assigned ephemeral ports.
    use super::{ForwardEntry, ForwardMap};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn empty_map() -> ForwardMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn insert_lookup_remove() {
        let m = empty_map();
        // Remote :3000, kernel handed back an ephemeral local port.
        let key = ("ws1".to_string(), 3000u16);
        m.lock().unwrap().insert(
            key.clone(),
            ForwardEntry {
                local_port: 49231,
                cancel: None,
            },
        );
        assert_eq!(m.lock().unwrap().get(&key).map(|e| e.local_port), Some(49231));
        let removed = m.lock().unwrap().remove(&key);
        assert!(removed.is_some());
        assert!(m.lock().unwrap().get(&key).is_none());
    }

    #[test]
    fn distinct_workspaces_same_remote_port_dont_collide() {
        // Two workspaces both expose remote :8080 — under 36.A each gets
        // its own kernel-assigned local port, so no collision.
        let m = empty_map();
        m.lock().unwrap().insert(
            ("a".to_string(), 8080),
            ForwardEntry { local_port: 49500, cancel: None },
        );
        m.lock().unwrap().insert(
            ("b".to_string(), 8080),
            ForwardEntry { local_port: 49777, cancel: None },
        );
        assert_eq!(m.lock().unwrap().len(), 2);
        assert_eq!(
            m.lock().unwrap().get(&("b".to_string(), 8080)).map(|e| e.local_port),
            Some(49777)
        );
    }
}

#[cfg(test)]
mod tcp_probe_tests {
    // Phase 46: tcp_probe is the post-bind sanity check inside
    // open_auto_forward — confirms a freshly bound local port is
    // actually reachable on 127.0.0.1 before we tell the FE the
    // forward is live (saves opening a browser tab on a dead port).
    use super::tcp_probe;
    use std::time::Duration;

    #[tokio::test]
    async fn probe_succeeds_for_listening_port() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = format!("127.0.0.1:{port}");
        // Accept loop in background so the probe's connect handshake completes.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let r = tcp_probe(&target, Duration::from_millis(500)).await;
        assert!(r.is_ok(), "expected Ok, got {:?}", r);
    }

    #[tokio::test]
    async fn probe_fails_for_vacant_port() {
        // Bind+drop reserves a port number then frees it; the probe
        // hits a port the OS just freed so it returns ECONNREFUSED
        // (immediate, no timeout needed).
        let port = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let target = format!("127.0.0.1:{port}");
        let r = tcp_probe(&target, Duration::from_millis(300)).await;
        assert!(r.is_err(), "expected Err for vacant port, got {:?}", r);
    }
}

#[cfg(test)]
mod migration_tests {
    // Phase 39.B: the auto_port_forward flip. MigrationFlags default
    // is exercised in settings.rs; here we test the data-level flip +
    // its idempotency.
    use super::{disable_all_auto_port_forward, Workspace, WorkspacesFile};

    fn ws(id: &str, apf: bool) -> Workspace {
        Workspace {
            id: id.to_string(),
            name: id.to_string(),
            color: None,
            emoji: None,
            cwd: None,
            connection: None,
            layout: None,
            setup_command: None,
            teardown_command: None,
            env: Vec::new(),
            auto_port_forward: apf,
            last_active_at: 0,
            git_worktree: None,
        }
    }

    #[test]
    fn flips_only_true_workspaces_and_is_idempotent() {
        let mut file = WorkspacesFile {
            workspaces: vec![ws("a", true), ws("b", false), ws("c", true)],
            ..Default::default()
        };
        // First run flips the two `true` ones.
        assert_eq!(disable_all_auto_port_forward(&mut file), 2);
        assert!(file.workspaces.iter().all(|w| !w.auto_port_forward));
        // Second run is a no-op — nothing left to flip.
        assert_eq!(disable_all_auto_port_forward(&mut file), 0);
    }

    #[test]
    fn empty_or_all_false_changes_nothing() {
        let mut empty = WorkspacesFile::default();
        assert_eq!(disable_all_auto_port_forward(&mut empty), 0);
        let mut all_off = WorkspacesFile {
            workspaces: vec![ws("a", false), ws("b", false)],
            ..Default::default()
        };
        assert_eq!(disable_all_auto_port_forward(&mut all_off), 0);
    }

    // Phase 53 (rebased): Browser/FileManager → Terminal rewrite.
    use super::{
        rewrite_browser_filemanager_panes_to_terminal, LayoutNode, PaneKind,
        SplitDirection,
    };

    fn pane(id: &str, kind: PaneKind) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.to_string(),
            pane_kind: kind,
            connection: None,
            browser: None,
            title: None,
            annotation: None,
            color: None,
            emoji: None,
            help_topic: None,
            diff_source: None,
            smart_bidi: None,
        }
    }

    fn ws_with_layout(id: &str, layout: LayoutNode) -> Workspace {
        Workspace {
            id: id.to_string(),
            name: id.to_string(),
            color: None,
            emoji: None,
            cwd: None,
            connection: None,
            layout: Some(layout),
            setup_command: None,
            teardown_command: None,
            env: Vec::new(),
            auto_port_forward: false,
            last_active_at: 0,
            git_worktree: None,
        }
    }

    #[test]
    #[allow(deprecated)]
    fn phase_53_rewrites_browser_and_filemanager_panes_and_is_idempotent() {
        // Nested layout: Split(Browser, Split(FileManager, Terminal))
        let inner = LayoutNode::Split {
            split_id: "s2".into(),
            direction: SplitDirection::Vertical,
            first: Box::new(pane("p2", PaneKind::FileManager)),
            second: Box::new(pane("p3", PaneKind::Terminal)),
            ratio: 0.5,
        };
        let layout = LayoutNode::Split {
            split_id: "s1".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("p1", PaneKind::Browser)),
            second: Box::new(inner),
            ratio: 0.5,
        };
        let mut file = WorkspacesFile {
            workspaces: vec![ws_with_layout("w1", layout)],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            2,
            "expected p1 (Browser) + p2 (FileManager) to be rewritten"
        );
        // Walk the migrated layout and confirm everything is Terminal.
        fn assert_all_terminal(n: &LayoutNode) {
            match n {
                LayoutNode::Pane { pane_kind, .. } => {
                    assert_eq!(*pane_kind, PaneKind::Terminal);
                }
                LayoutNode::Split { first, second, .. } => {
                    assert_all_terminal(first);
                    assert_all_terminal(second);
                }
            }
        }
        assert_all_terminal(file.workspaces[0].layout.as_ref().unwrap());
        // Second run is a no-op.
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }

    #[test]
    fn phase_53_leaves_help_and_diff_alone() {
        let layout = LayoutNode::Split {
            split_id: "s1".into(),
            direction: SplitDirection::Horizontal,
            first: Box::new(pane("p1", PaneKind::Help)),
            second: Box::new(pane("p2", PaneKind::Diff)),
            ratio: 0.5,
        };
        let mut file = WorkspacesFile {
            workspaces: vec![ws_with_layout("w1", layout)],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }

    #[test]
    fn phase_53_handles_workspace_with_no_layout() {
        let mut file = WorkspacesFile {
            workspaces: vec![Workspace {
                id: "w1".into(),
                name: "w1".into(),
                color: None,
                emoji: None,
                cwd: None,
                connection: None,
                layout: None,
                setup_command: None,
                teardown_command: None,
                env: Vec::new(),
                auto_port_forward: false,
                last_active_at: 0,
                git_worktree: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            rewrite_browser_filemanager_panes_to_terminal(&mut file),
            0
        );
    }
}

#[cfg(test)]
mod smart_connect_tests {
    // Phase 61: Smart Connect injection became shell-aware so local
    // PowerShell / Cmd panes can launch Claude Code too. POSIX must keep
    // its historical `exec` form byte-for-byte; the other two must not
    // contain `exec` (it doesn't exist there).
    use super::{build_smart_connect_script, ShellKind};

    #[test]
    fn posix_claude_keeps_exec_form() {
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "claude", None, None, None),
            "exec claude\r\n"
        );
        assert_eq!(
            build_smart_connect_script(
                ShellKind::Posix,
                "claude",
                Some("/home/x/my proj"),
                None,
                Some("--resume abc"),
            ),
            "cd '/home/x/my proj' && exec claude --resume abc\r\n"
        );
    }

    #[test]
    fn powershell_claude_no_exec_and_quotes_escaped() {
        assert_eq!(
            build_smart_connect_script(ShellKind::PowerShell, "claude", None, None, Some("--continue")),
            "claude --continue\r\n"
        );
        assert_eq!(
            build_smart_connect_script(
                ShellKind::PowerShell,
                "claude",
                Some(r"C:\Users\yo'si\code"),
                None,
                None,
            ),
            "Set-Location -LiteralPath 'C:\\Users\\yo''si\\code'; claude\r\n"
        );
    }

    #[test]
    fn cmd_claude_uses_cd_slash_d_and_strips_quotes() {
        assert_eq!(
            build_smart_connect_script(
                ShellKind::Cmd,
                "claude",
                Some(r#"D:\pro"j"#),
                None,
                None,
            ),
            "cd /d \"D:\\proj\" && claude\r\n"
        );
    }

    #[test]
    fn cmd_mode_runs_command_and_empty_returns_nothing() {
        assert_eq!(
            build_smart_connect_script(ShellKind::PowerShell, "cmd", None, Some("npm run dev"), None),
            "npm run dev\r\n"
        );
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "cmd", None, Some("htop"), None),
            "exec htop\r\n"
        );
        // Empty / whitespace command → nothing to inject, even with a cwd.
        assert_eq!(
            build_smart_connect_script(ShellKind::Cmd, "cmd", Some(r"C:\x"), Some("  "), None),
            ""
        );
        // Unknown mode → nothing.
        assert_eq!(
            build_smart_connect_script(ShellKind::Posix, "default", None, None, None),
            ""
        );
    }
}
