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

use std::path::PathBuf;
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
