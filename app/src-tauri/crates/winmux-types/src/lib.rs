//! Phase 51.A: pure data types extracted from `app/src-tauri/src/lib.rs`.
//!
//! This crate holds the wire-format / persistence-format types that the
//! rest of the winmux backend (and the future split crates for ssh,
//! pty, feed, workspaces, rpc) all reference. By keeping them here:
//!
//! - The `app` crate doesn't grow new copies as features land.
//! - Future MCP server / CLI subcrates can depend on this type set
//!   without pulling in the full Tauri runtime.
//! - ts-rs binding regeneration is isolated to one place.
//!
//! Intentionally no business logic: just structs, enums, serde attrs,
//! and the small helper functions that serde attrs reference by name
//! (`default_true`, `is_true`, `is_terminal_kind`). Default impls that
//! belong to a type also stay with it.
//!
//! Note: `WorkspacesFile`, `CreateInput`, and the runtime types
//! (`AppState`, `Session`, etc.) intentionally stayed in `app` for the
//! 51.A POC — they're either internal-shaped or tightly coupled to
//! tokio/AppState. Later phase splits will pull more out.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Connection ─────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Connection {
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

// ─── SplitDirection ─────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

// ─── PaneKind ───────────────────────────────────────────────────────

// Phase 8.A: pane kind. Defaults to Terminal so older workspaces.json
// (no `pane_kind` field) deserialize unchanged. `is_terminal_kind`
// lets serde elide the field on terminal panes, keeping legacy
// round-trips byte-identical.
#[derive(Clone, Copy, Serialize, Deserialize, Default, Debug, PartialEq, Eq, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    /// Phase 24.D: the removed ClaudeChat / ClaudeLog variants
    /// (Phase 22 / 24.B) are aliased to Terminal so older
    /// workspaces.json files load cleanly — those panes become
    /// Terminals via `backfill_terminal_connections` in
    /// `load_from_disk`. Three competing "talk to claude" UIs felt
    /// fragmented; Yossi paused the unified-view goal pending a
    /// future rebuild.
    #[default]
    #[serde(
        alias = "claudechat",
        alias = "claude_chat",
        alias = "ClaudeChat",
        alias = "claudelog",
        alias = "claude_log",
        alias = "ClaudeLog"
    )]
    Terminal,
    Browser,
    /// Phase 15.B: dual-column file manager (local + remote SFTP).
    /// The pane itself carries no remote state — it picks up the
    /// workspace's SSH session at runtime, so a file-manager pane in
    /// an SSH workspace lights up the right column only after a
    /// terminal pane in that workspace has authenticated.
    #[serde(rename = "filemanager", alias = "file_manager", alias = "FileManager")]
    FileManager,
    /// Phase 33: in-app help pane. Renders a markdown document
    /// keyed by `help_topic` on the pane node (e.g. "ssh-key-setup").
    /// Carries no remote state — entirely local, no SSH/PTY.
    Help,
    /// Phase 50 (#2.4): live unified-diff view of a workspace's git
    /// repo. The pane node carries an optional `diff_source` that
    /// selects working-vs-index, working-vs-HEAD, or working-vs-<ref>.
    /// A background watcher polls `git diff` every ~800ms and emits a
    /// `diff-pane-updated` event when the output hash changes.
    Diff,
}

/// Helper for `LayoutNode::Pane.pane_kind` serde elision. Lives here
/// (not in `app`) because serde resolves the function name in the
/// local scope of the type using the attr.
pub fn is_terminal_kind(k: &PaneKind) -> bool {
    matches!(k, PaneKind::Terminal)
}

// ─── DiffSource ─────────────────────────────────────────────────────

// Phase 50: which diff a Diff pane shows. Default = Working (git diff
// with no ref → working tree vs index). `#[serde(tag = "kind")]` so
// the JSON shape mirrors PaneKind's pattern and is easy to switch on
// in TypeScript.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffSource {
    Working,
    Head,
    Ref { git_ref: String },
}

impl Default for DiffSource {
    fn default() -> Self {
        DiffSource::Working
    }
}

// ─── BrowserState ───────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct BrowserState {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
    // Phase 8.B: when true (default) and the pane lives in an SSH workspace,
    // navigate-resolve rewrites localhost:N / 127.0.0.1:N to a forwarded local
    // listener. The address bar still shows the original URL.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub forward_localhost: bool,
    // Phase 8.C fix: URL the iframe most recently fired `load` for. Lets
    // `browser-wait` return immediately when the page is already loaded
    // instead of timing out waiting for the next load event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_loaded_url: Option<String>,
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

pub fn default_true() -> bool {
    true
}
pub fn is_true(b: &bool) -> bool {
    *b
}

pub const BROWSER_HISTORY_MAX: usize = 50;

// ─── LayoutNode ─────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LayoutNode {
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
        // Phase 24.D: removed chat / claudelog fields with the
        // ClaudeChat (Phase 22) and ClaudeLog (Phase 24.B) panes.
        // Any leftover objects under those keys in an existing
        // workspaces.json are silently dropped by serde (no
        // `deny_unknown_fields`) and the converted Terminal pane
        // gets a fresh connection via `backfill_terminal_connections`
        // in load_from_disk.
        // Phase 7.A: optional human-readable annotations on each leaf. Both fields
        // serialize-skip when None so existing workspaces.json files round-trip
        // unchanged until the user edits one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation: Option<String>,
        // Phase 31: per-pane identity. None = inherit from the parent
        // workspace's identity (set in Phase 30). The workspace value
        // is what shows in the sidebar/accent strip; the pane value
        // takes precedence in the pane header + window title when set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emoji: Option<String>,
        // Phase 33: which help topic this pane shows (only meaningful
        // for `pane_kind == Help`). Same Option pattern as `browser` —
        // present for Help panes, absent for everything else.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        help_topic: Option<String>,
        // Phase 50: which diff the Diff pane shows. Present for Diff
        // panes, absent for everything else. None on a Diff pane is
        // treated as DiffSource::Working by the backend watcher.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_source: Option<DiffSource>,
        // Phase 52 (BiDi 33B): opt-in PTY-stream bidi filter. None or
        // Some(false) = passthrough; Some(true) = inject FSI/PDI around
        // Latin runs near RTL context. Persists across reloads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        smart_bidi: Option<bool>,
    },
    Split {
        split_id: String,
        direction: SplitDirection,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
        ratio: f32,
    },
}

// ─── EnvVar ─────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

// ─── Workspace ──────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../../src/bindings/")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    // Phase 30: per-workspace emoji glyph. Free-form (up to 16 UTF-8 bytes),
    // typically a single grapheme cluster. Renders as a prefix on the
    // sidebar tab and in the OS window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    // Phase 23.D: canonical workspace-level connection. Populated on
    // create AND on load (back-filled from the first Terminal pane's
    // connection if absent). Pane-level `connection` is kept as an
    // optional override for back-compat, but everything that needs
    // to spawn a session (pane_connect / split fallback / FE dropdown)
    // falls through to this field when the pane has no own value.
    // Reason: enforces "SSH workspace never produces a local shell"
    // and lets non-Terminal panes (FileManager / Browser / ClaudeChat)
    // reconnect to the workspace's intended target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Connection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutNode>,
    // Phase 7.C: per-workspace shell automation. Sent into the spawned shell after a
    // small delay (so the shell has finished printing its banner). `env` is exported
    // first, then `setup_command` runs. `teardown_command` is sent right before
    // disconnect with a brief grace period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvVar>,
    // Phase 36 (#2.2) → 39: when true, winmux auto-opens SSH
    // local-forwards for listening ports the remote watcher reports.
    // Phase 39: default flipped to FALSE — users opt in per-workspace
    // (Settings / Ports window). Avoids a flood of forwards on connect
    // and the WINMUX-CHALLENGE foot-gun. `#[serde(default)]` → bool
    // false, so older workspaces.json without the field load as OFF.
    #[serde(default)]
    pub auto_port_forward: bool,
    // Phase 49-C: unix-seconds timestamp of the last user activation.
    // Used by the optional auto-destroy sweep at startup. Updated in
    // workspace_set_active. `#[serde(default)]` so existing
    // workspaces.json files load with 0 (treated as "unknown / very
    // old" by the sweep — never deletes on the first run, only after
    // the workspace has been activated at least once this session).
    #[serde(default)]
    pub last_active_at: u64,
    // Phase 49-B: if Some, this workspace is anchored to a git worktree
    // path created by `workspace_create_worktree`. The UI shows a 🌿
    // chip on the workspace tab. The path is what `cd`s into when a
    // pane spawns; it sits alongside (not inside) the original repo so
    // it never collides with the user's working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree: Option<PathBuf>,
}
