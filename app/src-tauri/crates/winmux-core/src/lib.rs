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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
pub fn dlog(msg: &str) {
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
