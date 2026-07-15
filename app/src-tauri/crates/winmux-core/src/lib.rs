//! Phase 51.B: core infrastructure shared across the winmux backend crates.
//!
//! This crate holds the cross-cutting concerns the rest of the codebase
//! (current `app`, plus future `winmux-ssh`/`-tunnel`/`-pty`/`-rpc`) all
//! reach for: the user-visible debug log, shell-quote helper, and the
//! pure layout-tree walkers that have no state and no I/O.
//!
//! 51.B is being landed in incremental sub-commits (51.B1, 51.B2, …)
//! rather than as one ~5,000-LOC move, so intermediate states stay
//! green and the build is never left broken between commits.
//!
//! Things explicitly NOT in this crate (yet): SshClient + russh
//! handler impl, Session/SshSession types, ForwardEntry, AppState +
//! CoreState. Those land in subsequent 51.B sub-commits.

// beta.3 (netfree): shared HTTP retry helper for the updater path.
// See http.rs — GET-only, jittered exponential backoff on transport errors.
pub mod http;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use portable_pty::{ChildKiller, MasterPty};
use russh::client;
use russh::Channel;
use russh_keys::HashAlg;
use serde::{Deserialize, Serialize};
use winmux_types::{Connection, LayoutNode, PaneKind};

// ─── config dir + debug.log ──────────────────────────────────────────

/// Resolve the per-user winmux config directory.
///
/// `WINMUX_CONFIG_DIR` env var wins if set (used by tests + isolated
/// debug builds, see `winmux-debug-test\run-winmux-debug.bat`).
/// Otherwise: `dirs::config_dir() / "winmux"` (≈ `%APPDATA%\winmux\`).
/// The directory is created on demand.
pub fn config_dir() -> Result<PathBuf, String> {
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

/// Documented alias for `config_dir` so external module callsites
/// have a stable name. (Historical: this used to be private in lib.rs
/// with `config_dir_pub` as the cross-module surface; keeping the
/// alias means every existing callsite continues to resolve.)
pub fn config_dir_pub() -> Result<PathBuf, String> {
    config_dir()
}

/// User-visible debug log. Writes a timestamped line to
/// `<config_dir>/debug.log`. Errors are intentionally swallowed —
/// logging must never crash the caller. See CLAUDE.md Rule 9 for the
/// dlog-vs-tracing audience distinction.
/// Size cap for `debug.log` before it rotates to `debug.log.1`. Bounds the
/// on-disk footprint to ~2× this (current + one rotation) so a chatty session
/// can't balloon the log — the v0.3.1 pipe-leak produced ~936k lines.
pub const DEBUG_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn dlog(msg: &str) {
    if let Ok(dir) = config_dir() {
        let p = dir.join("debug.log");
        // Rotate once the active log passes the cap (cheap: one stat per line;
        // dlog already does an open/write/close per call).
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.len() > DEBUG_LOG_MAX_BYTES {
                let _ = std::fs::rename(&p, dir.join("debug.log.1"));
            }
        }
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

/// User-visible debug log, tagged by subsystem so the single `debug.log` reads
/// as organized per-service streams — e.g. `[MONITOR]`, `[MOBILE]`, `[ADDON]`,
/// `[TUNNEL]`, `[SSH]`. The tag is uppercased for at-a-glance scanning. Prefer
/// this over bare `dlog` for new subsystem logging. See CLAUDE.md Rule 9 for
/// the dlog-vs-tracing audience distinction.
pub fn dlog_tag(subsystem: &str, msg: &str) {
    dlog(&format!("[{}] {msg}", subsystem.to_uppercase()));
}

/// Phase 75: prune debug logs so they can't accumulate. Deletes the rotated
/// `debug.log.1` once it's older than `retention_days`, and if the primary
/// `debug.log` itself hasn't been touched within the window (app unused for a
/// while), clears it for a fresh start. `retention_days == 0` disables pruning
/// (keep forever). Called once at startup. Best-effort — never fails the boot.
pub fn prune_logs(retention_days: u32) {
    if retention_days == 0 {
        return;
    }
    let dir = match config_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let cutoff = match std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(retention_days) * 86_400))
    {
        Some(c) => c,
        None => return,
    };
    let stale = |p: &std::path::Path| -> bool {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|mt| mt < cutoff)
            .unwrap_or(false)
    };
    // Rotated file older than the window → delete outright.
    let rotated = dir.join("debug.log.1");
    if rotated.exists() && stale(&rotated) {
        let _ = std::fs::remove_file(&rotated);
    }
    // Primary log untouched for > retention (a stale session) → truncate fresh.
    let primary = dir.join("debug.log");
    if primary.exists() && stale(&primary) {
        let _ = std::fs::write(&primary, b"");
    }
}

/// Phase 75: clear the debug log now (Settings → Logs "Clear" button).
/// Truncates `debug.log` and removes the rotated `debug.log.1`.
pub fn clear_debug_log() -> Result<(), String> {
    let dir = config_dir()?;
    std::fs::write(dir.join("debug.log"), b"").map_err(|e| format!("clear debug.log: {e}"))?;
    let _ = std::fs::remove_file(dir.join("debug.log.1"));
    Ok(())
}

// ─── shell escape ────────────────────────────────────────────────────

/// Minimal POSIX single-quote escape. Wraps the value in single quotes
/// and rewrites any internal single-quote as `'\''`. Safe for
/// /bin/sh-style. Per Absolute Rule #3, used wherever we must inject
/// caller-supplied strings into a remote shell command.
pub fn shell_quote(s: &str) -> String {
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

// ─── pure LayoutNode walkers ─────────────────────────────────────────

/// Flat-collect every pane id under the given subtree (depth-first).
pub fn collect_panes(node: &LayoutNode, out: &mut Vec<String>) {
    match node {
        LayoutNode::Pane { pane_id, .. } => out.push(pane_id.clone()),
        LayoutNode::Split { first, second, .. } => {
            collect_panes(first, out);
            collect_panes(second, out);
        }
    }
}

/// Phase 8.E: visit every leaf pane and report its kind to the callback.
/// Used by the `dev.get-state` summary builder.
pub fn collect_panes_with_kind(node: &LayoutNode, f: &mut dyn FnMut(PaneKind)) {
    match node {
        LayoutNode::Pane { pane_kind, .. } => f(*pane_kind),
        LayoutNode::Split { first, second, .. } => {
            collect_panes_with_kind(first, f);
            collect_panes_with_kind(second, f);
        }
    }
}

/// First Terminal-pane connection found in DFS order, if any.
/// Module-private in lib.rs; here we keep it `pub` for cross-crate use
/// but consumers should prefer `first_terminal_connection_pub` which
/// is the documented surface.
pub fn first_terminal_connection(node: &LayoutNode) -> Option<Connection> {
    match node {
        LayoutNode::Pane {
            pane_kind,
            connection,
            ..
        } if matches!(pane_kind, PaneKind::Terminal) => connection.clone(),
        LayoutNode::Pane { .. } => None,
        LayoutNode::Split { first, second, .. } => {
            first_terminal_connection(first).or_else(|| first_terminal_connection(second))
        }
    }
}

/// Documented alias for `first_terminal_connection` so external module
/// callsites have a stable name. (Phase 23.D introduced the
/// `_pub` suffix when this used to be private; keeping the alias means
/// every existing callsite continues to resolve.)
pub fn first_terminal_connection_pub(node: &LayoutNode) -> Option<Connection> {
    first_terminal_connection(node)
}

/// Phase 23.D: fix-up loop run at load_from_disk time.
/// Walks the workspace's layout tree and, for every Terminal pane with
/// no `connection`, fills it from the workspace-level fallback (the
/// first sibling Terminal pane's connection, or `Local{shell:None}`).
/// Returns the patched node plus a bool telling the caller whether the
/// tree was actually mutated (so persistence can be marked dirty).
pub fn backfill_terminal_connections(
    node: LayoutNode,
    workspace_conn: &Option<Connection>,
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
            let needs_fix =
                matches!(pane_kind, PaneKind::Terminal) && connection.is_none();
            let new_conn = if needs_fix {
                Some(
                    workspace_conn
                        .clone()
                        .unwrap_or(Connection::Local { shell: None }),
                )
            } else {
                connection
            };
            (
                LayoutNode::Pane {
                    pane_id,
                    pane_kind,
                    connection: new_conn,
                    browser,
                    title,
                    annotation,
                    color,
                    emoji,
                    help_topic,
                    diff_source,
                    smart_bidi,
                },
                needs_fix,
            )
        }
        LayoutNode::Split {
            split_id,
            direction,
            first,
            second,
            ratio,
        } => {
            let (new_first, c1) = backfill_terminal_connections(*first, workspace_conn);
            let (new_second, c2) = backfill_terminal_connections(*second, workspace_conn);
            (
                LayoutNode::Split {
                    split_id,
                    direction,
                    first: Box::new(new_first),
                    second: Box::new(new_second),
                    ratio,
                },
                c1 || c2,
            )
        }
    }
}

// ─── Known-hosts (TOFU) ──────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct KnownHost {
    #[serde(rename = "type")]
    pub key_type: String,
    pub fingerprint: String,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct KnownHostsFile {
    #[serde(default)]
    pub hosts: HashMap<String, KnownHost>,
}

pub fn known_hosts_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("known_hosts.json"))
}

pub fn load_known_hosts() -> KnownHostsFile {
    if let Ok(p) = known_hosts_path() {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(f) = serde_json::from_str::<KnownHostsFile>(&text) {
                return f;
            }
        }
    }
    KnownHostsFile::default()
}

pub fn save_known_hosts(file: &KnownHostsFile) -> Result<(), String> {
    let path = known_hosts_path()?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Default, Clone, Debug)]
pub struct HostCheckOutcome {
    pub fingerprint: String,
    pub key_type: String,
    pub matched: bool,
    pub is_unknown: bool,
    pub mismatch_old: Option<String>,
}

// ─── SshClient (russh Handler) ───────────────────────────────────────

/// Callback type for forwarded-tcpip bridge. The app crate (or a
/// future `winmux-tunnel`) supplies a closure; `winmux-core` calls it
/// when the SSH server forwards a connection back to us. Phase 51.B2
/// option β: this is how we break the SshClient → tunnel circular dep
/// without folding tunnel into core.
pub type BridgeSpawner = Arc<dyn Fn(Channel<client::Msg>, Arc<String>) + Send + Sync>;

pub struct SshClient {
    pub target: String,
    pub accept_unknown: bool,
    pub result: Arc<Mutex<HostCheckOutcome>>,
    /// If set, the handler accepts forwarded-tcpip channels and bridges
    /// them via `bridge_spawner` after validating this token on the
    /// first line.
    pub tunnel_token: Option<Arc<String>>,
    /// Phase 51.B2: caller-injected spawner so this crate avoids a
    /// dep on winmux-tunnel. Forwarded channels are dropped if either
    /// `tunnel_token` or `bridge_spawner` is None.
    pub bridge_spawner: Option<BridgeSpawner>,
}

impl SshClient {
    /// Construct a tolerant client for one-shot operations (the connect
    /// wizard test, provisioning steps). Accepts any server key,
    /// doesn't touch known_hosts, no tunnel token / spawner. The
    /// host-check outcome is captured but never persisted.
    pub fn new_anonymous(target: String) -> Self {
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
            bridge_spawner: None,
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
    /// Phase 51.B2: the actual bridge spawn is delegated to a caller-injected
    /// closure so winmux-core stays decoupled from the tunnel impl.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        match (self.tunnel_token.clone(), self.bridge_spawner.clone()) {
            (Some(token), Some(spawn)) => spawn(channel, token),
            _ => tracing::warn!(
                "forwarded-tcpip channel arrived but no tunnel_token/bridge_spawner set; dropping"
            ),
        }
        Ok(())
    }
}

// ─── Session types ───────────────────────────────────────────────────

/// Either a local PTY-backed session or an SSH-backed one. AppState's
/// `sessions` map (Phase 51.B5 will pull this into CoreState) is
/// keyed by session id; pane operations look up the matching variant
/// and dispatch.
pub enum Session {
    Local(LocalSession),
    Ssh(SshSession),
}

pub struct LocalSession {
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
}

pub struct SshSession {
    /// Phase 41: `None` for a headless session — one established by
    /// `workspace_ensure_connected` to back the tmux picker / file manager
    /// with no PTY behind it. Pane-backed sessions always carry `Some`.
    pub tx: Option<tokio::sync::mpsc::UnboundedSender<SshCmd>>,
    /// Phase 8.B: shared russh client handle. The I/O task and any port-forward
    /// accept loop both hold an Arc; russh's Handle methods take &self, so
    /// concurrent users send commands through the underlying mpsc sender.
    pub handle: Arc<client::Handle<SshClient>>,
    /// Phase 8.B: workspace this session belongs to, so port-forward bookkeeping
    /// can clean up when the workspace is deleted or all SSH sessions exit.
    pub workspace_id: String,
    /// Phase 11.A: when this session was started with `persistent=true` we wrap
    /// the shell in a tmux attach-or-create. Storing the name lets us send
    /// `tmux kill-session -t NAME` via a separate exec channel on demand.
    pub tmux_session: Option<String>,
    /// Phase 23.C: connection metadata so we can rehydrate a `Connection`
    /// value from a live session — used by `live_ssh_connection_for_workspace`
    /// when the user adds a new terminal pane to an SSH workspace whose
    /// connection details no longer live in any pane (e.g. all terminals
    /// closed but a FileManager pane kept the SSH handle alive).
    pub host: String,
    pub user: String,
    pub port: u16,
    pub key_path: Option<String>,
    /// beta.3 (netfree, Track 1b): set by the io-loop when the SSH transport
    /// drops so a background reconnect task can announce itself to the UI
    /// and reject a second cascading drop-emit if the same session flaps
    /// twice in quick succession. Cleared by the `ssh_cancel_reconnect`
    /// Tauri command or when the reconnect flow completes / gives up.
    /// Arc<AtomicBool> so multiple `_for_task` clones share one flag.
    pub reconnecting: Arc<AtomicBool>,
}

impl SshSession {
    /// Phase 41: forward a command to the PTY task. Headless sessions have
    /// no PTY (`tx == None`), so this is a no-op for them. Pane operations
    /// only ever look sessions up by pane id, so in practice this only
    /// reaches `Some` senders — the `None` arm is the safety net.
    pub fn try_send(&self, cmd: SshCmd) -> Result<(), String> {
        match &self.tx {
            Some(tx) => tx.send(cmd).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

#[derive(Debug)]
pub enum SshCmd {
    Data(Vec<u8>),
    Resize(u32, u32),
    Kill,
}

// ─── Forward bookkeeping ─────────────────────────────────────────────

/// Phase 8.B: SSH local port forwards (browser pane → remote dev server).
/// Key = (workspace_id, remote_port). Value carries the local listener port
/// and a oneshot to cancel the accept loop on cleanup.
pub struct ForwardEntry {
    pub local_port: u16,
    pub cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

pub type ForwardMap = Arc<Mutex<HashMap<(String, u16), ForwardEntry>>>;
pub type SessionMap = Arc<Mutex<HashMap<String, Session>>>;
pub type PaneSessionMap = Arc<Mutex<HashMap<String, String>>>;

// ─── pipe_name ───────────────────────────────────────────────────────

/// Phase 51.C: the per-user Windows Named Pipe path that the RPC
/// server binds to and the remote tunnel bridges into. Lives in
/// winmux-core so both winmux-tunnel (bridge_to_pipe) and the future
/// winmux-rpc (server bind) can reach it without depending on each
/// other.
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\winmux-{}", user)
}

// ─── CoreState ───────────────────────────────────────────────────────

/// Phase 51.B4: the russh/PTY/forward runtime state that every future
/// split crate (tunnel, bootstrap, ssh, pty, rpc) will need, factored
/// out of AppState so winmux-core owns it instead of app. The outer
/// `AppState` (in `app/lib.rs`) holds a `core: CoreState` plus the
/// fields that depend on tauri / notes / settings / dev modules.
///
/// All fields are `Arc<Mutex<…>>` and `Clone`able, so cloning
/// CoreState (e.g. for spawning a tokio task that holds its own
/// reference) only clones the Arcs, not the data behind them.
#[derive(Default, Clone)]
pub struct CoreState {
    pub sessions: SessionMap,
    pub pane_sessions: PaneSessionMap,
    pub forwards: ForwardMap,
    pub port_watchers: Arc<Mutex<std::collections::HashSet<String>>>,
    pub internal_reverse_tunnel_remote_ports:
        Arc<Mutex<HashMap<String, std::collections::HashSet<u16>>>>,
    pub detected_ports:
        Arc<Mutex<HashMap<String, HashMap<u16, (String, String)>>>>,
    pub port_watcher_tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pub workspace_tunnel_tokens: Arc<Mutex<HashMap<String, Arc<String>>>>,
    pub diff_pane_watchers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

// ─── Phase 59: pure-function unit tests ──────────────────────────────
//
// Targets the layout walkers + shell_quote + pipe_name. These are
// hot-path helpers — a regression here would break SSH command
// injection of caller-supplied strings, mis-fill pane connections
// on load, or send the remote tunnel to the wrong named-pipe path.

#[cfg(test)]
mod tests {
    use super::*;
    use winmux_types::{Connection, LayoutNode, PaneKind, SplitDirection};

    // ── helpers ────────────────────────────────────────────────────

    fn pane(id: &str, kind: PaneKind, conn: Option<Connection>) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.into(),
            pane_kind: kind,
            connection: conn,
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

    fn split(id: &str, dir: SplitDirection, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            split_id: id.into(),
            direction: dir,
            first: Box::new(first),
            second: Box::new(second),
            ratio: 0.5,
        }
    }

    // ── shell_quote (Absolute Rule #3 helper) ──────────────────────

    #[test]
    fn shell_quote_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_quote_simple_alphanumeric() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("foo123"), "'foo123'");
    }

    #[test]
    fn shell_quote_path_with_slashes_unchanged() {
        // Slashes don't need escaping inside single quotes.
        assert_eq!(
            shell_quote("/home/yossi/.ssh/id_ed25519"),
            "'/home/yossi/.ssh/id_ed25519'"
        );
    }

    #[test]
    fn shell_quote_embedded_single_quote_uses_close_quote_escape() {
        // The classic POSIX trick: close the quote, insert a backslash-
        // quote, reopen. The end result is the literal four chars '\''.
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn shell_quote_multiple_single_quotes() {
        assert_eq!(shell_quote("'a''b'"), r#"''\''a'\'''\''b'\'''"#);
    }

    #[test]
    fn shell_quote_dangerous_metachars_safe() {
        // Inside single quotes, $/`/!/;/&/|/space/newline/backslash are
        // ALL literal. The threat model here is command injection on
        // the remote shell; verifying the escape leaves them inert.
        assert_eq!(
            shell_quote("$(rm -rf /); echo pwn"),
            "'$(rm -rf /); echo pwn'"
        );
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        assert_eq!(shell_quote("a`b`c"), "'a`b`c'");
    }

    // ── collect_panes / collect_panes_with_kind ────────────────────

    #[test]
    fn collect_panes_single_leaf() {
        let n = pane("p1", PaneKind::Terminal, None);
        let mut out = Vec::new();
        collect_panes(&n, &mut out);
        assert_eq!(out, vec!["p1".to_string()]);
    }

    #[test]
    fn collect_panes_dfs_order() {
        // Tree:    s_outer
        //         /        \
        //    s_inner        p3
        //    /     \
        //   p1     p2
        // DFS-first should produce [p1, p2, p3].
        let tree = split(
            "s_outer",
            SplitDirection::Vertical,
            split(
                "s_inner",
                SplitDirection::Horizontal,
                pane("p1", PaneKind::Terminal, None),
                pane("p2", PaneKind::Terminal, None),
            ),
            pane("p3", PaneKind::Terminal, None),
        );
        let mut out = Vec::new();
        collect_panes(&tree, &mut out);
        assert_eq!(out, vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn collect_panes_with_kind_visits_every_leaf() {
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("a", PaneKind::Terminal, None),
            split(
                "s2",
                SplitDirection::Vertical,
                pane("b", PaneKind::Diff, None),
                pane("c", PaneKind::Help, None),
            ),
        );
        let mut kinds: Vec<PaneKind> = Vec::new();
        collect_panes_with_kind(&tree, &mut |k| kinds.push(k));
        assert_eq!(kinds, vec![PaneKind::Terminal, PaneKind::Diff, PaneKind::Help]);
    }

    // ── first_terminal_connection ──────────────────────────────────

    #[test]
    fn first_terminal_connection_none_when_no_terminal_panes() {
        let tree = pane("h", PaneKind::Help, None);
        assert!(first_terminal_connection(&tree).is_none());
    }

    #[test]
    fn first_terminal_connection_skips_non_terminal_panes() {
        // Non-terminal pane in DFS-first slot must be skipped; the
        // search continues into the second subtree to find a real
        // Terminal pane's connection.
        let ssh = Connection::Ssh {
            host: "h".into(),
            user: "u".into(),
            port: 22,
            key_path: None,
        };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("help", PaneKind::Help, None),
            pane("term", PaneKind::Terminal, Some(ssh.clone())),
        );
        let found = first_terminal_connection(&tree).expect("should find SSH");
        // Pattern-match — Connection has no Debug.
        match found {
            Connection::Ssh { host, .. } => assert_eq!(host, "h"),
            _ => panic!("expected SSH"),
        }
    }

    #[test]
    fn first_terminal_connection_skips_orphan_and_finds_real_connection() {
        // A Terminal pane with no connection returns None from the
        // Pane arm; the Split arm's `or_else` falls through to the
        // second subtree. So the walker effectively finds the first
        // Terminal pane that ACTUALLY has a connection in DFS order.
        // (Phase 23.D documented this as the "second tier" of the
        // four-tier fallback chain for split_pane_in.)
        let ssh = Connection::Ssh {
            host: "h2".into(),
            user: "u".into(),
            port: 22,
            key_path: None,
        };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("orphan", PaneKind::Terminal, None),
            pane("realssh", PaneKind::Terminal, Some(ssh)),
        );
        let found = first_terminal_connection(&tree).expect("should find SSH on right");
        match found {
            Connection::Ssh { host, .. } => assert_eq!(host, "h2"),
            _ => panic!("expected SSH from the second subtree"),
        }
    }

    #[test]
    fn first_terminal_connection_returns_none_when_all_terminals_orphaned() {
        // No connection anywhere → walker returns None (and the
        // caller falls back to Connection::Local{shell:None} via
        // split_pane_in's tier-4 default).
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane("orphan1", PaneKind::Terminal, None),
            pane("orphan2", PaneKind::Terminal, None),
        );
        assert!(first_terminal_connection(&tree).is_none());
    }

    // ── backfill_terminal_connections ──────────────────────────────

    #[test]
    fn backfill_does_nothing_when_no_terminal_panes_lack_connection() {
        let conn = Connection::Local { shell: None };
        let tree = pane("p1", PaneKind::Terminal, Some(conn));
        let (new_tree, changed) =
            backfill_terminal_connections(tree, &Some(Connection::Local { shell: None }));
        assert!(!changed, "no missing connection → no backfill");
        // pane_id preserved.
        match new_tree {
            LayoutNode::Pane { pane_id, .. } => assert_eq!(pane_id, "p1"),
            _ => panic!("should still be Pane"),
        }
    }

    #[test]
    fn backfill_fills_missing_terminal_pane_from_workspace_conn() {
        // Phase 23.D scenario: a Terminal pane whose connection field
        // is None must inherit the workspace-level fallback.
        let ws_conn = Connection::Ssh {
            host: "ws-host".into(),
            user: "ws-user".into(),
            port: 22,
            key_path: None,
        };
        let tree = pane("p1", PaneKind::Terminal, None);
        let (new_tree, changed) = backfill_terminal_connections(tree, &Some(ws_conn));
        assert!(changed, "missing connection should be backfilled");
        match new_tree {
            LayoutNode::Pane {
                connection: Some(Connection::Ssh { host, .. }),
                ..
            } => assert_eq!(host, "ws-host"),
            _ => panic!("connection should be filled with workspace SSH"),
        }
    }

    #[test]
    fn backfill_falls_back_to_local_when_no_workspace_conn() {
        // No workspace_conn → backfill uses Local{shell:None} so a
        // Terminal pane never ends up unconnectable.
        let tree = pane("p1", PaneKind::Terminal, None);
        let (new_tree, changed) = backfill_terminal_connections(tree, &None);
        assert!(changed);
        match new_tree {
            LayoutNode::Pane {
                connection: Some(Connection::Local { shell }),
                ..
            } => assert!(shell.is_none()),
            _ => panic!("should be Local fallback"),
        }
    }

    #[test]
    fn backfill_recurses_into_splits_changed_flag_or() {
        // changed == c1 || c2 — if only the inner subtree needed a fix
        // the bool should still propagate.
        let ws_conn = Connection::Local { shell: None };
        let tree = split(
            "s",
            SplitDirection::Horizontal,
            pane(
                "good",
                PaneKind::Terminal,
                Some(Connection::Local { shell: None }),
            ),
            pane("orphan", PaneKind::Terminal, None),
        );
        let (_new_tree, changed) = backfill_terminal_connections(tree, &Some(ws_conn));
        assert!(changed, "orphan side needed backfill → changed=true");
    }

    #[test]
    fn backfill_leaves_non_terminal_panes_alone() {
        // A Help pane with no connection is correct — only Terminal
        // panes get the fix-up.
        let tree = pane("h", PaneKind::Help, None);
        let (new_tree, changed) = backfill_terminal_connections(
            tree,
            &Some(Connection::Local { shell: None }),
        );
        assert!(!changed);
        match new_tree {
            LayoutNode::Pane {
                connection,
                pane_kind,
                ..
            } => {
                assert!(matches!(pane_kind, PaneKind::Help));
                assert!(connection.is_none());
            }
            _ => panic!("should still be Pane"),
        }
    }

    // ── pipe_name ──────────────────────────────────────────────────

    #[test]
    fn pipe_name_prefixes_correctly() {
        let name = pipe_name();
        assert!(
            name.starts_with(r"\\.\pipe\winmux-"),
            "expected Windows pipe prefix, got {name}"
        );
        // Whatever USERNAME / whoami returns, it shouldn't be empty.
        assert!(name.len() > r"\\.\pipe\winmux-".len());
    }

    // ── iso_now ────────────────────────────────────────────────────

    #[test]
    fn iso_now_has_z_suffix_and_seconds_precision() {
        let s = iso_now();
        // RFC 3339 with SecondsFormat::Secs + use_z=true: e.g.
        // "2026-06-09T05:14:00Z".
        assert!(s.ends_with('Z'), "expected Z suffix, got {s}");
        // No fractional seconds (use Secs precision).
        assert!(!s.contains('.'), "no fractional seconds expected, got {s}");
        assert_eq!(s.len(), 20, "expected 20-char RFC 3339, got {s}");
    }
}
