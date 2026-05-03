// winmux CLI client.
//
// Transport selection:
// - On Windows, by default the CLI talks to the running winmux app over a per-user
//   named pipe at `\\.\pipe\winmux-<USER>`. Override with the `WINMUX_PIPE_PATH` env var.
// - On Linux/Unix (and as a Windows fallback when set), the CLI connects over TCP using
//   the address in `WINMUX_SOCKET_ADDR` (e.g. `127.0.0.1:8765`). This is the path used
//   when the binary runs on a remote SSH server tunneled back to a local listener.
// - If a Linux build can't find `WINMUX_SOCKET_ADDR`, it errors with exit code 2.

mod hooks;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::process::ExitCode;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Parser, Debug)]
#[command(
    name = "winmux",
    version,
    about = "winmux CLI client (talks to a running winmux app via named pipe or TCP)"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,

    /// Print raw RPC response (single-line JSON) instead of pretty.
    #[arg(long, global = true)]
    raw: bool,
    /// Suppress normal output on success (errors still printed).
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    ListWorkspaces,
    SelectWorkspace {
        #[arg(long)]
        id: String,
    },
    NewWorkspace {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "local")]
        r#type: String,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        key_path: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        color: Option<String>,
        /// Phase 7.C: command run after the shell prompt is ready.
        #[arg(long)]
        setup: Option<String>,
        /// Phase 7.C: command sent right before disconnect.
        #[arg(long)]
        teardown: Option<String>,
        /// Phase 7.C: env var (KEY=VALUE). Repeat for multiple.
        #[arg(long = "env")]
        env: Vec<String>,
    },

    /// Update an existing workspace's editable fields. Phase 7.C.
    UpdateWorkspace {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        setup: Option<String>,
        #[arg(long)]
        teardown: Option<String>,
        /// Repeat for each var. Passing --env without any clears all.
        #[arg(long = "env")]
        env: Vec<String>,
        /// Force-replace env even if no --env flags given (default behavior is
        /// "leave env alone if no --env flag was passed").
        #[arg(long)]
        clear_env: bool,
    },
    DeleteWorkspace {
        #[arg(long)]
        id: String,
    },
    Send {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        data: String,
    },
    SendKey {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        key: String,
    },
    Notify {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        workspace_id: Option<String>,
    },
    Tree {
        #[arg(long)]
        workspace_id: Option<String>,
    },
    SetStatus {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        text: String,
    },

    /// Set a persistent title on a pane (Phase 7.A). Pass an empty string to clear.
    SetPaneTitle {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        title: String,
    },

    /// Set a persistent free-text annotation on a pane (Phase 7.A). Empty clears.
    SetPaneAnnotation {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        annotation: String,
    },

    /// Phase 8.A: split a pane (terminal or browser).
    Split {
        /// Pane id of the existing pane to split off.
        #[arg(long)]
        pane: String,
        /// `right`/`horizontal` (default) or `down`/`vertical`.
        #[arg(long, default_value = "right")]
        direction: String,
        /// `terminal` (default) inherits the pane's connection; `browser` opens an iframe.
        #[arg(long, default_value = "terminal")]
        kind: String,
        /// Initial URL when --kind=browser. Defaults to about:blank.
        #[arg(long)]
        url: Option<String>,
    },

    /// Phase 8.A: navigate a browser pane to a new URL (history is pushed).
    BrowserNavigate {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        url: String,
    },

    /// Phase 8.A: pop the browser pane's history once.
    BrowserGoBack {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.A: reset the browser pane to its home URL.
    BrowserGoHome {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.B: resolve a URL through the workspace's port-forward map.
    /// Opens a forward if needed; prints the rewritten URL. Useful for agents
    /// that want to share a URL with the user that actually works on Windows.
    BrowserResolveUrl {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        url: String,
    },

    /// Phase 8.C: read the persisted current URL of a browser pane.
    BrowserUrl {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.C: read the navigation history of a browser pane.
    BrowserHistory {
        #[arg(long)]
        pane: String,
    },

    /// Phase 8.C: block until the iframe fires onload (default 10s). Returns
    /// the loaded URL on success.
    BrowserWait {
        #[arg(long)]
        pane: String,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.1 (replaces the original 8.C eval): evaluate JS inside the
    /// iframe via the postMessage bridge and return the result. Works on
    /// cross-origin pages — the bridge runs in every frame at document
    /// creation time.
    BrowserEval {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        expr: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.C: capture a screenshot of the pane (html2canvas). With --output,
    /// writes a PNG to disk; otherwise prints the data URL. Cross-origin iframe
    /// content renders as blanks under html2canvas.
    BrowserScreenshot {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value_t = 15_000)]
        timeout_ms: u64,
    },

    /// Phase 8.E: developer / introspection tools. See `winmux dev --help`.
    Dev {
        #[command(subcommand)]
        op: DevOp,
    },

    /// Phase 8.F.1: click a CSS-selector match in a browser pane's iframe.
    /// Works on cross-origin pages (the bridge script runs in every frame).
    BrowserClick {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        selector: String,
        /// "left" (default) or "right"
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.2: semantic element search inside the iframe. Filters AND
    /// together — at least one must be specified. Returns matching elements
    /// with synthesized stable selectors usable for browser-click / type.
    BrowserFind {
        #[arg(long)]
        pane: String,
        /// ARIA role: button, link, textbox, checkbox, heading, listitem, ...
        #[arg(long)]
        role: Option<String>,
        /// Visible text content (case-insensitive contains)
        #[arg(long)]
        text: Option<String>,
        /// Accessible label (`aria-label` or `<label for>`)
        #[arg(long)]
        label: Option<String>,
        /// `placeholder` attribute (case-insensitive contains)
        #[arg(long)]
        placeholder: Option<String>,
        /// `alt` attribute on images
        #[arg(long)]
        alt: Option<String>,
        /// `title` attribute
        #[arg(long)]
        title: Option<String>,
        /// `data-testid` attribute (exact match)
        #[arg(long)]
        testid: Option<String>,
        /// Raw CSS selector to narrow the search pool before filters run.
        #[arg(long)]
        selector: Option<String>,
        /// Skip elements with display:none / visibility:hidden / zero area.
        #[arg(long)]
        visible_only: bool,
        /// Return only the first match.
        #[arg(long)]
        first: bool,
        /// Cap on number of matches returned.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.3a: poll the iframe until the criteria are met or timeout.
    /// At least one criterion must be specified; multiple AND together.
    /// Default state is `visible` — the matched element must also be visible.
    BrowserWaitFor {
        #[arg(long)]
        pane: String,
        /// CSS selector to find.
        #[arg(long)]
        selector: Option<String>,
        /// Visible text content (deepest-match, same as browser-find).
        #[arg(long)]
        text: Option<String>,
        /// ARIA role.
        #[arg(long)]
        role: Option<String>,
        /// Accessible label (`aria-label` or `<label for>`).
        #[arg(long)]
        label: Option<String>,
        /// `data-testid` attribute (exact).
        #[arg(long)]
        testid: Option<String>,
        /// Substring the iframe's `window.location.href` must contain.
        #[arg(long)]
        url_contains: Option<String>,
        /// `visible` (default) | `attached` | `hidden` | `detached`. Detached
        /// inverts: succeeds when NO element matches.
        #[arg(long, default_value = "visible")]
        state: String,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.2: simplified accessibility-flavored DOM tree of the iframe.
    /// JSON by default; --text renders as a YAML-like outline.
    BrowserSnapshot {
        #[arg(long)]
        pane: String,
        #[arg(long, default_value_t = 50)]
        max_depth: usize,
        /// Strip non-essential attributes (level/url/src/alt/name) — keeps
        /// only role + text + children.
        #[arg(long)]
        text_only: bool,
        /// Render the tree as an indented YAML-ish outline instead of JSON.
        #[arg(long)]
        text: bool,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Phase 8.F.1: type text into an input/textarea matched by CSS selector.
    BrowserType {
        #[arg(long)]
        pane: String,
        #[arg(long)]
        selector: String,
        #[arg(long)]
        text: String,
        #[arg(long)]
        clear_first: bool,
        #[arg(long, default_value_t = 5_000)]
        timeout_ms: u64,
    },

    /// Stub for Claude Code agent hooks: reads JSON from stdin, fires a notify.
    ClaudeHook {
        subcommand: String,
    },

    /// Quick-capture notes (Phase 7.B). See `winmux note --help` for subcommands.
    Note {
        #[command(subcommand)]
        op: NoteOp,
    },

    /// Register agent hooks (e.g. Claude Code's hooks.json) so AI agents pipe
    /// permission requests + lifecycle events through winmux. Idempotent and additive.
    SetupHooks {
        /// Which agent's config to install. `claude` (default) or `all`.
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Print what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Replace any existing winmux hook entries even if already registered.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum DevOp {
    /// Snapshot of app state: version, git hash, workspaces summary, active
    /// sessions, tunnel state, feed/notes counts, debug.log tail, console tail.
    GetState {
        /// Pretty-print JSON (default for `winmux dev` is compact).
        #[arg(long)]
        pretty: bool,
        /// Human-readable summary instead of JSON.
        #[arg(long)]
        text: bool,
    },
    /// Last N captured frontend console events (errors + warns).
    ConsoleTail {
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Last N lines of `<appdata>/winmux/debug.log`.
    DebugLogTail {
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
    /// Capture a bug report (state snapshot + description) under
    /// `<appdata>/winmux/bug-reports/bug-<unix>.json`. Reads description from
    /// stdin (terminate with empty line + Ctrl-Z+Enter on Windows / Ctrl-D on
    /// Unix) when --description is omitted.
    ReportBug {
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        repro_steps: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum NoteOp {
    /// Add a new note. Tag is optional — try `idea`, `bug`, `todo`, or your own.
    Add {
        /// Free-text body. Wrap in quotes if it contains spaces.
        text: String,
        #[arg(long)]
        tag: Option<String>,
        /// Workspace id to associate with the note (auto-detected from --pane if not set).
        #[arg(long)]
        workspace: Option<String>,
        /// Pane id to associate with the note (defaults to $WINMUX_PANE_ID env if set).
        #[arg(long)]
        pane: Option<String>,
    },
    /// List notes. Defaults to open notes only; pass --done or --all to include resolved.
    List {
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        done: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Mark a note as done.
    Done {
        id: String,
    },
    /// Delete a note.
    Delete {
        id: String,
    },
    /// Update text/tag/status of a note.
    Update {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// "open" or "done".
        #[arg(long)]
        status: Option<String>,
    },
}

#[cfg(windows)]
fn default_pipe_name() -> String {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| whoami::username());
    format!(r"\\.\pipe\winmux-{}", user)
}

/// Load env vars from `$HOME/.winmux/run/last.env` if the relevant ones aren't already set.
/// Phase 6.3: written by the Windows app for each SSH workspace as a fallback for
/// sshd configurations that strip per-channel env vars.
fn load_fallback_env_file() {
    if std::env::var("WINMUX_SOCKET_ADDR").is_ok() {
        return;
    }
    let home = match std::env::var_os("HOME") {
        Some(h) => h,
        None => return,
    };
    let path = std::path::Path::new(&home).join(".winmux/run/last.env");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}

// Phase 8.F.2: render an accessibility snapshot as a YAML-ish outline.
fn render_snapshot_text(node: &Value, depth: usize, out: &mut String) {
    if node.is_null() {
        out.push_str("(empty tree)\n");
        return;
    }
    let pad = "  ".repeat(depth);
    let role = node.get("role").and_then(|v| v.as_str()).unwrap_or("?");
    let text = node.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let level = node.get("level").and_then(|v| v.as_u64());
    let url = node.get("url").and_then(|v| v.as_str());
    let mut head = format!("{}- {}", pad, role);
    if let Some(l) = level {
        head.push_str(&format!("[{}]", l));
    }
    if !text.is_empty() {
        head.push_str(&format!(": \"{}\"", text.replace('\n', " ").replace('\r', "")));
    } else if let Some(name) = node.get("name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            head.push_str(&format!(": \"{}\"", name));
        }
    }
    if let Some(u) = url {
        head.push_str(&format!(" → {}", u));
    }
    head.push('\n');
    out.push_str(&head);
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for c in children {
            render_snapshot_text(c, depth + 1, out);
        }
    }
}

// Phase 8.E: render `winmux dev get-state` as a short human summary instead
// of dumping the full JSON. Used when --text is passed.
fn render_dev_state_text(v: &Value) -> String {
    let mut out = String::new();
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("?").to_string();
    out.push_str(&format!("winmux {} ({})\n", s("version"), s("git_hash")));
    out.push_str(&format!("appdata: {}\n", s("appdata_dir")));
    if let Some(ws) = v.get("workspaces") {
        out.push_str(&format!(
            "workspaces: {} (active: {})\n",
            ws.get("count").and_then(|x| x.as_u64()).unwrap_or(0),
            ws.get("active_id")
                .and_then(|x| x.as_str())
                .unwrap_or("none")
        ));
    }
    if let Some(sessions) = v.get("sessions").and_then(|x| x.as_array()) {
        out.push_str(&format!("active sessions: {}\n", sessions.len()));
        for s in sessions {
            out.push_str(&format!(
                "  pane={} kind={} conn={}\n",
                s.get("pane_id").and_then(|x| x.as_str()).unwrap_or("?"),
                s.get("kind").and_then(|x| x.as_str()).unwrap_or("?"),
                s.get("connection_type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
            ));
        }
    }
    if let Some(forwards) = v
        .get("tunnels")
        .and_then(|t| t.get("browser_forwards"))
        .and_then(|x| x.as_array())
    {
        out.push_str(&format!("port forwards: {}\n", forwards.len()));
        for f in forwards {
            out.push_str(&format!(
                "  ws={} remote={} -> local={}\n",
                f.get("workspace_id").and_then(|x| x.as_str()).unwrap_or("?"),
                f.get("remote_port").and_then(|x| x.as_u64()).unwrap_or(0),
                f.get("local_port").and_then(|x| x.as_u64()).unwrap_or(0),
            ));
        }
    }
    if let Some(notes) = v.get("notes") {
        out.push_str(&format!(
            "notes: {} open / {} done\n",
            notes.get("open").and_then(|x| x.as_u64()).unwrap_or(0),
            notes.get("done").and_then(|x| x.as_u64()).unwrap_or(0),
        ));
    }
    if let Some(feed) = v.get("feed") {
        out.push_str(&format!(
            "feed: {} open / {} done\n",
            feed.get("open").and_then(|x| x.as_u64()).unwrap_or(0),
            feed.get("done").and_then(|x| x.as_u64()).unwrap_or(0),
        ));
    }
    if let Some(log) = v.get("log_tail").and_then(|x| x.as_array()) {
        out.push_str(&format!("debug.log: {} tail lines\n", log.len()));
    }
    if let Some(c) = v.get("console_tail").and_then(|x| x.as_array()) {
        out.push_str(&format!("console: {} captured events\n", c.len()));
    }
    out
}

async fn rpc_call(method: &str, params: Value) -> Result<Value, String> {
    load_fallback_env_file();

    // Prefer TCP if WINMUX_SOCKET_ADDR is set (works on any OS, including remote tunnels).
    if let Ok(addr) = std::env::var("WINMUX_SOCKET_ADDR") {
        let stream = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("connect tcp {}: {}", addr, e))?;
        let token = std::env::var("WINMUX_TUNNEL_TOKEN").ok();
        return rpc_via(stream, method, params, token.as_deref()).await;
    }

    // Otherwise on Windows, use a named pipe.
    #[cfg(windows)]
    {
        let name = std::env::var("WINMUX_PIPE_PATH").unwrap_or_else(|_| default_pipe_name());
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .map_err(|e| {
                format!(
                    "connect to {}: {} (is the winmux app running?)",
                    name, e
                )
            })?;
        return rpc_via(pipe, method, params, None).await;
    }

    #[cfg(not(windows))]
    {
        Err("no transport configured: set WINMUX_SOCKET_ADDR=host:port".into())
    }
}

/// Phase 6.4: HMAC-SHA256 challenge-response. The token never travels on the wire;
/// only the random nonce (sent by the server) and the HMAC of it (sent by the client).
async fn perform_handshake<R, W>(
    reader: &mut tokio::io::BufReader<R>,
    writer: &mut W,
    token: &str,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    // Read challenge.
    let mut line = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut line),
    )
    .await;
    match r {
        Ok(Ok(0)) => return Err("server closed before challenge".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read challenge: {e}")),
        Err(_) => return Err("challenge read timed out".into()),
    }
    let trimmed = line.trim();
    let nonce_hex = trimmed
        .strip_prefix("WINMUX-CHALLENGE ")
        .ok_or_else(|| format!("expected WINMUX-CHALLENGE, got {:?}", trimmed))?;
    let nonce = hex_decode(nonce_hex)?;

    // Compute HMAC and respond.
    let mut mac = HmacSha256::new_from_slice(token.as_bytes())
        .map_err(|e| format!("hmac key: {e}"))?;
    mac.update(&nonce);
    let response = mac.finalize().into_bytes();
    let resp_line = format!("WINMUX-RESPONSE {}\n", hex_encode(&response));
    writer
        .write_all(resp_line.as_bytes())
        .await
        .map_err(|e| format!("write response: {e}"))?;
    writer.flush().await.ok();

    // Read OK / DENIED.
    let mut ok = String::new();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reader.read_line(&mut ok),
    )
    .await;
    match r {
        Ok(Ok(0)) => return Err("server closed before verdict".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read verdict: {e}")),
        Err(_) => return Err("verdict timed out".into()),
    }
    let verdict = ok.trim();
    if verdict == "WINMUX-OK" {
        Ok(())
    } else if let Some(reason) = verdict.strip_prefix("WINMUX-DENIED") {
        Err(format!("auth denied:{}", reason))
    } else {
        Err(format!("unexpected handshake verdict: {:?}", verdict))
    }
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(hex_digit(x >> 4));
        s.push(hex_digit(x & 0xf));
    }
    s
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '?',
    }
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(format!("odd-length hex ({})", bytes.len()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("bad hex char: {:?}", c as char)),
    }
}

async fn rpc_via<S>(
    stream: S,
    method: &str,
    params: Value,
    token: Option<&str>,
) -> Result<Value, String>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf = BufReader::new(reader);

    // Phase 6.4: TCP transport authenticates via HMAC challenge-response. Token never
    // appears on the wire. Pipe transport (Windows) skips this — the pipe's per-user
    // ACL is the auth.
    if let Some(t) = token {
        perform_handshake(&mut buf, &mut writer, t).await?;
    }

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let line = format!("{}\n", req);
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.flush().await.ok();
    let mut response = String::new();
    buf.read_line(&mut response)
        .await
        .map_err(|e| e.to_string())?;
    let resp: Value = serde_json::from_str(response.trim())
        .map_err(|e| format!("bad response: {} ({})", e, response))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("rpc error: {}", err));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

fn derive_hook_title(subcommand: &str, payload: &Value) -> String {
    match subcommand {
        "tool-permission" | "pre-tool-use" => {
            if let Some(cmd) = payload.get("command").and_then(|v| v.as_str()) {
                format!("Run `{}` ?", cmd)
            } else if let Some(tool) = payload.get("tool").and_then(|v| v.as_str()) {
                format!("Allow `{}` ?", tool)
            } else if let Some(t) = payload.get("title").and_then(|v| v.as_str()) {
                t.to_string()
            } else {
                format!("agent: {}", subcommand)
            }
        }
        "session-start" => "Claude session started".to_string(),
        "session-stop" | "session-end" => "Claude session ended".to_string(),
        "session-idle" => "Claude is idle".to_string(),
        "session-active" => "Claude is active".to_string(),
        "notification" => payload
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "Claude notification".to_string()),
        "prompt-submit" => "Prompt submitted".to_string(),
        other => format!("agent: {}", other),
    }
}

fn derive_hook_summary(_subcommand: &str, payload: &Value) -> String {
    if let Some(s) = payload.get("summary").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = payload.get("description").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(s) = payload.get("body").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(reason) = payload.get("reason").and_then(|v| v.as_str()) {
        return reason.to_string();
    }
    let s = serde_json::to_string(payload).unwrap_or_default();
    if s.len() > 280 {
        format!("{}…", truncate_at_char_boundary(&s, 280))
    } else {
        s
    }
}

/// Truncate at or before `max_bytes`, never inside a multi-byte UTF-8
/// character. The naive `&s[..max_bytes]` panics for Hebrew / Arabic / CJK
/// (and emoji) when `max_bytes` lands in the middle of a code-point —
/// observed in the wild when Claude Code sent a Stop hook with a Hebrew
/// `last_assistant_message`.
pub(crate) fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Parse `KEY=VALUE` repeats into the JSON shape the backend expects.
/// Errors out if any entry has no `=`.
fn parse_env_flags(flags: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(flags.len());
    for f in flags {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| format!("--env argument {:?} is missing '='", f))?;
        if k.is_empty() {
            return Err(format!("--env argument {:?} has empty key", f));
        }
        out.push(json!({ "key": k, "value": v }));
    }
    Ok(out)
}

fn build_connection(
    type_: &str,
    shell: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: u16,
    key_path: Option<String>,
) -> Result<Value, String> {
    match type_ {
        "local" => Ok(json!({ "type": "local", "shell": shell })),
        "ssh" => {
            let host = host.ok_or("ssh requires --host")?;
            let user = user.ok_or("ssh requires --user")?;
            Ok(json!({
                "type": "ssh",
                "host": host,
                "user": user,
                "port": port,
                "key_path": key_path,
            }))
        }
        other => Err(format!("unknown type: {} (expected local|ssh)", other)),
    }
}

// Phase 8.F.2 fix: Windows debug builds give the main thread a 1 MB stack.
// Clap's derive macro for our 30+ subcommands (especially `BrowserFind` with
// 13 Option fields) generates a lot of format-string state that — combined
// with tokio's runtime + serde — overflows that 1 MB during arg parsing on
// some invocations. Spawn the real work on a worker thread with an 8 MB
// stack and join.
fn main() -> ExitCode {
    match std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(real_main)
        .and_then(|h| h.join().map_err(|_| std::io::Error::other("worker panicked")))
    {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: worker thread spawn/join failed: {e}");
            ExitCode::from(2)
        }
    }
}

#[tokio::main]
async fn real_main() -> ExitCode {
    let cli = Cli::parse();
    let result: Result<Value, String> = match &cli.command {
        Cmd::ListWorkspaces => rpc_call("list-workspaces", json!({})).await,
        Cmd::SelectWorkspace { id } => rpc_call("select-workspace", json!({ "id": id })).await,
        Cmd::NewWorkspace {
            name,
            r#type,
            shell,
            host,
            user,
            port,
            key_path,
            cwd,
            color,
            setup,
            teardown,
            env,
        } => {
            let conn = match build_connection(
                r#type,
                shell.clone(),
                host.clone(),
                user.clone(),
                *port,
                key_path.clone(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            };
            let env_pairs = match parse_env_flags(env) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(2);
                }
            };
            rpc_call(
                "new-workspace",
                json!({
                    "name": name,
                    "connection": conn,
                    "cwd": cwd,
                    "color": color,
                    "setup_command": setup,
                    "teardown_command": teardown,
                    "env": env_pairs,
                }),
            )
            .await
        }
        Cmd::UpdateWorkspace {
            id,
            name,
            color,
            cwd,
            setup,
            teardown,
            env,
            clear_env,
        } => {
            let mut params = json!({ "workspace_id": id });
            if let Some(v) = name {
                params["name"] = json!(v);
            }
            if let Some(v) = color {
                params["color"] = json!(v);
            }
            if let Some(v) = cwd {
                params["cwd"] = json!(v);
            }
            if let Some(v) = setup {
                params["setup_command"] = json!(v);
            }
            if let Some(v) = teardown {
                params["teardown_command"] = json!(v);
            }
            // env: only send if user passed --env or --clear-env.
            if !env.is_empty() || *clear_env {
                let env_pairs = match parse_env_flags(env) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {}", e);
                        return ExitCode::from(2);
                    }
                };
                params["env"] = json!(env_pairs);
            }
            rpc_call("update-workspace", params).await
        }
        Cmd::DeleteWorkspace { id } => rpc_call("delete-workspace", json!({ "id": id })).await,
        Cmd::Send { pane, data } => {
            rpc_call("send", json!({ "pane_id": pane, "data": data })).await
        }
        Cmd::SendKey { pane, key } => {
            rpc_call("send-key", json!({ "pane_id": pane, "key": key })).await
        }
        Cmd::Notify {
            title,
            body,
            workspace_id,
        } => {
            rpc_call(
                "notify",
                json!({
                    "title": title,
                    "body": body,
                    "workspace_id": workspace_id,
                }),
            )
            .await
        }
        Cmd::Tree { workspace_id } => {
            rpc_call("tree", json!({ "workspace_id": workspace_id })).await
        }
        Cmd::SetStatus { pane, text } => {
            rpc_call("set-status", json!({ "pane_id": pane, "text": text })).await
        }
        Cmd::SetPaneTitle { pane, title } => {
            rpc_call(
                "set-pane-title",
                json!({ "pane_id": pane, "title": title }),
            )
            .await
        }
        Cmd::SetPaneAnnotation { pane, annotation } => {
            rpc_call(
                "set-pane-annotation",
                json!({ "pane_id": pane, "annotation": annotation }),
            )
            .await
        }
        Cmd::Split {
            pane,
            direction,
            kind,
            url,
        } => {
            rpc_call(
                "split",
                json!({
                    "pane_id": pane,
                    "direction": direction,
                    "kind": kind,
                    "url": url,
                }),
            )
            .await
        }
        Cmd::BrowserNavigate { pane, url } => {
            rpc_call(
                "pane.browser.navigate",
                json!({ "pane_id": pane, "url": url }),
            )
            .await
        }
        Cmd::BrowserGoBack { pane } => {
            rpc_call("pane.browser.go-back", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserGoHome { pane } => {
            rpc_call("pane.browser.go-home", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserResolveUrl { pane, url } => {
            rpc_call(
                "pane.browser.resolve-url",
                json!({ "pane_id": pane, "url": url }),
            )
            .await
        }
        Cmd::BrowserUrl { pane } => rpc_call("pane.browser.url", json!({ "pane_id": pane })).await,
        Cmd::BrowserHistory { pane } => {
            rpc_call("pane.browser.history", json!({ "pane_id": pane })).await
        }
        Cmd::BrowserWait { pane, timeout_ms } => {
            rpc_call(
                "pane.browser.wait",
                json!({ "pane_id": pane, "timeout_ms": timeout_ms }),
            )
            .await
        }
        Cmd::BrowserEval {
            pane,
            expr,
            timeout_ms,
        } => {
            // Phase 8.F.1: route to the new iframe bridge instead of the
            // old `pane.browser.eval` (which used the html2canvas-era
            // frontend listener and just returned a "cross-origin needs
            // 8.D" error). Strict upgrade: the bridge actually returns the
            // value, on cross-origin pages too.
            rpc_call(
                "pane.browser.iframe.eval",
                json!({
                    "pane_id": pane,
                    "expression": expr,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserScreenshot {
            pane,
            output,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.screenshot",
                json!({
                    "pane_id": pane,
                    "output_path": output,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserFind {
            pane,
            role,
            text,
            label,
            placeholder,
            alt,
            title,
            testid,
            selector,
            visible_only,
            first,
            limit,
            timeout_ms,
        } => {
            // Build the params map directly so unspecified fields stay absent
            // (the bridge skips empty filters, but snake/camel case matters).
            let mut p = serde_json::Map::new();
            p.insert("pane_id".into(), json!(pane));
            p.insert("timeout_ms".into(), json!(timeout_ms));
            if let Some(v) = role { p.insert("role".into(), json!(v)); }
            if let Some(v) = text { p.insert("text".into(), json!(v)); }
            if let Some(v) = label { p.insert("label".into(), json!(v)); }
            if let Some(v) = placeholder { p.insert("placeholder".into(), json!(v)); }
            if let Some(v) = alt { p.insert("alt".into(), json!(v)); }
            if let Some(v) = title { p.insert("title".into(), json!(v)); }
            if let Some(v) = testid { p.insert("testid".into(), json!(v)); }
            if let Some(v) = selector { p.insert("selector".into(), json!(v)); }
            if *visible_only { p.insert("visibleOnly".into(), json!(true)); }
            if *first { p.insert("first".into(), json!(true)); }
            if let Some(v) = limit { p.insert("limit".into(), json!(v)); }
            rpc_call("pane.browser.iframe.find", Value::Object(p)).await
        }
        Cmd::BrowserWaitFor {
            pane,
            selector,
            text,
            role,
            label,
            testid,
            url_contains,
            state,
            timeout_ms,
        } => {
            let mut p = serde_json::Map::new();
            p.insert("pane_id".into(), json!(pane));
            p.insert("timeout_ms".into(), json!(timeout_ms));
            p.insert("state".into(), json!(state));
            if let Some(v) = selector { p.insert("selector".into(), json!(v)); }
            if let Some(v) = text { p.insert("text".into(), json!(v)); }
            if let Some(v) = role { p.insert("role".into(), json!(v)); }
            if let Some(v) = label { p.insert("label".into(), json!(v)); }
            if let Some(v) = testid { p.insert("testid".into(), json!(v)); }
            if let Some(v) = url_contains { p.insert("urlContains".into(), json!(v)); }
            rpc_call("pane.browser.iframe.wait-for", Value::Object(p)).await
        }
        Cmd::BrowserSnapshot {
            pane,
            max_depth,
            text_only,
            text,
            timeout_ms,
        } => {
            match rpc_call(
                "pane.browser.iframe.snapshot",
                json!({
                    "pane_id": pane,
                    "maxDepth": max_depth,
                    "textOnly": text_only,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
            {
                Ok(res) => {
                    if *text {
                        let tree = res.get("tree").cloned().unwrap_or(Value::Null);
                        let mut out = String::new();
                        render_snapshot_text(&tree, 0, &mut out);
                        print!("{}", out);
                        std::process::exit(0);
                    }
                    Ok(res)
                }
                Err(e) => Err(e),
            }
        }
        Cmd::BrowserClick {
            pane,
            selector,
            button,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.iframe.click",
                json!({
                    "pane_id": pane,
                    "selector": selector,
                    "button": button,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::BrowserType {
            pane,
            selector,
            text,
            clear_first,
            timeout_ms,
        } => {
            rpc_call(
                "pane.browser.iframe.type",
                json!({
                    "pane_id": pane,
                    "selector": selector,
                    "text": text,
                    "clear_first": clear_first,
                    "timeout_ms": timeout_ms,
                }),
            )
            .await
        }
        Cmd::Dev { op } => match op {
            DevOp::GetState { pretty: _, text } => {
                match rpc_call("dev.get-state", json!({})).await {
                    Ok(v) => {
                        if *text {
                            // Bypass the JSON-pretty pipeline and print directly.
                            print!("{}", render_dev_state_text(&v));
                            std::process::exit(0);
                        }
                        Ok(v)
                    }
                    Err(e) => Err(e),
                }
            }
            DevOp::ConsoleTail { limit } => {
                rpc_call("dev.console-tail", json!({ "limit": limit })).await
            }
            DevOp::DebugLogTail { limit } => {
                rpc_call("dev.debug-log-tail", json!({ "limit": limit })).await
            }
            DevOp::ReportBug {
                description,
                repro_steps,
            } => {
                use std::io::Read;
                let desc = match description {
                    Some(d) => Ok(d.clone()),
                    None => {
                        eprintln!("Describe the issue (Ctrl-Z then Enter to finish on Windows, Ctrl-D on Unix):");
                        let mut s = String::new();
                        std::io::stdin()
                            .read_to_string(&mut s)
                            .map_err(|e| format!("read stdin: {e}"))
                            .map(|_| s.trim().to_string())
                    }
                };
                match desc {
                    Ok(d) if !d.is_empty() => {
                        rpc_call(
                            "dev.report-bug",
                            json!({
                                "description": d,
                                "repro_steps": repro_steps,
                            }),
                        )
                        .await
                    }
                    Ok(_) => Err("description is required".into()),
                    Err(e) => Err(e),
                }
            }
        },
        Cmd::Note { op } => match op {
            NoteOp::Add {
                text,
                tag,
                workspace,
                pane,
            } => {
                let pane_eff = pane
                    .clone()
                    .or_else(|| std::env::var("WINMUX_PANE_ID").ok())
                    .filter(|s| !s.is_empty());
                rpc_call(
                    "note-add",
                    json!({
                        "text": text,
                        "tag": tag,
                        "workspace_id": workspace,
                        "pane_id": pane_eff,
                    }),
                )
                .await
            }
            NoteOp::List {
                tag,
                done,
                all,
                workspace,
                limit,
                json: as_json,
            } => {
                let status = if *all {
                    None
                } else if *done {
                    Some("done".to_string())
                } else {
                    Some("open".to_string())
                };
                let result = rpc_call(
                    "note-list",
                    json!({
                        "tag": tag,
                        "status": status,
                        "workspace_id": workspace,
                        "limit": limit,
                    }),
                )
                .await;
                match result {
                    Ok(v) => {
                        if *as_json || cli.raw {
                            let s = if cli.raw {
                                serde_json::to_string(&v).unwrap_or_default()
                            } else {
                                serde_json::to_string_pretty(&v).unwrap_or_default()
                            };
                            println!("{}", s);
                            return ExitCode::SUCCESS;
                        }
                        let arr = v.as_array().cloned().unwrap_or_default();
                        if arr.is_empty() {
                            println!("(no notes)");
                            return ExitCode::SUCCESS;
                        }
                        for n in arr {
                            let id = n.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                            let st =
                                n.get("status").and_then(|x| x.as_str()).unwrap_or("open");
                            let tg = n.get("tag").and_then(|x| x.as_str()).unwrap_or("-");
                            let upd = n
                                .get("updated_at")
                                .and_then(|x| x.as_str())
                                .unwrap_or("");
                            let txt = n.get("text").and_then(|x| x.as_str()).unwrap_or("");
                            let mark = if st == "done" { "✓" } else { " " };
                            let snippet = if txt.len() > 80 {
                                format!("{}…", truncate_at_char_boundary(txt, 80))
                            } else {
                                txt.to_string()
                            };
                            println!(
                                "{}  {:<8}  [{}]  {}  {}",
                                mark, tg, &id[..id.len().min(20)], upd, snippet
                            );
                        }
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        return ExitCode::from(2);
                    }
                }
            }
            NoteOp::Done { id } => rpc_call("note-done", json!({ "id": id })).await,
            NoteOp::Delete { id } => rpc_call("note-delete", json!({ "id": id })).await,
            NoteOp::Update {
                id,
                text,
                tag,
                status,
            } => {
                let mut params = json!({ "id": id });
                if let Some(t) = text {
                    params["text"] = json!(t);
                }
                if let Some(tg) = tag {
                    params["tag"] = json!(tg);
                }
                if let Some(s) = status {
                    params["status"] = json!(s);
                }
                rpc_call("note-update", params).await
            }
        },
        Cmd::ClaudeHook { subcommand } => {
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            let payload: Value = if buf.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(buf.trim()).unwrap_or_else(|_| json!({ "raw": buf.trim() }))
            };

            let blocking = matches!(subcommand.as_str(), "tool-permission" | "pre-tool-use");
            let kind = if blocking { "permission_request" } else { "passive" };

            let request_id = format!(
                "req_{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let pane_id = std::env::var("WINMUX_PANE_ID").ok();
            let title = derive_hook_title(subcommand, &payload);
            let summary = derive_hook_summary(subcommand, &payload);

            // Honor `wait_timeout_seconds` from the agent's payload if present and sane.
            // Default 120, clamped to [1, 600] to bound server-side resource holds.
            let timeout_secs = payload
                .get("wait_timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(1, 600);
            // Phase setup-hooks-fix v2: keep stderr clean. Claude Code's UI
            // surfaces stderr and our diagnostic line was cluttering the chat.
            // The same data is already captured server-side via the feed.push
            // RPC and shows up in `winmux dev debug-log-tail`.

            let result = rpc_call(
                "feed.push",
                json!({
                    "request_id": request_id,
                    "kind": kind,
                    "subkind": subcommand,
                    "pane_id": pane_id,
                    "title": title,
                    "summary": summary,
                    "payload": payload,
                    "wait_timeout_seconds": timeout_secs,
                }),
            )
            .await;

            match result {
                Ok(v) => {
                    let decision = v
                        .get("decision")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    // Phase setup-hooks-fix v2: emit Claude Code v2.1+ hook
                    // output format. Per https://docs.claude.com/en/docs/claude-code/hooks
                    //   { "hookSpecificOutput": { "hookEventName": ..., "permissionDecision": "allow"|"deny"|"ask", "permissionDecisionReason"? } }
                    // exit 0 + the JSON ABOVE is the in-band signaling — exit
                    // codes 1/2/3 are NOT how decisions are expressed. The
                    // legacy `tool-permission` subcommand keeps the old shape
                    // so any pre-existing custom flow doesn't break.
                    match subcommand.as_str() {
                        "pre-tool-use" => {
                            let (perm, reason) = match decision {
                                "allow" | "passive" => ("allow", None),
                                "deny" => ("deny", Some("User denied via winmux".to_string())),
                                "timeout" => (
                                    "deny",
                                    Some(
                                        "winmux permission request timed out — denying conservatively"
                                            .to_string(),
                                    ),
                                ),
                                other => (
                                    "ask",
                                    Some(format!("winmux returned unknown decision: {other}")),
                                ),
                            };
                            let mut hso = json!({
                                "hookEventName": "PreToolUse",
                                "permissionDecision": perm,
                            });
                            if let Some(r) = reason {
                                hso["permissionDecisionReason"] = json!(r);
                            }
                            let out = json!({ "hookSpecificOutput": hso });
                            println!("{}", serde_json::to_string(&out).unwrap_or_default());
                            return ExitCode::SUCCESS;
                        }
                        "notification" | "session-start" | "session-end" | "stop" => {
                            // Passive lifecycle hooks: silent ack. exit 0, no
                            // stdout — Claude Code does not need a structured
                            // response for these.
                            return ExitCode::SUCCESS;
                        }
                        _ => {
                            // Legacy `tool-permission` or unknown subcommands:
                            // print the raw RPC payload and use the historical
                            // exit-code-based signaling so anything that scrapes
                            // the JSON or branches on $? keeps working.
                            if !cli.quiet {
                                let s = if cli.raw {
                                    serde_json::to_string(&v).unwrap_or_default()
                                } else {
                                    serde_json::to_string_pretty(&v).unwrap_or_default()
                                };
                                println!("{}", s);
                            }
                            return match decision {
                                "allow" | "passive" => ExitCode::SUCCESS,
                                "deny" => ExitCode::from(1),
                                "timeout" => ExitCode::from(2),
                                _ => ExitCode::from(3),
                            };
                        }
                    }
                }
                Err(e) => {
                    // Real wire error — keep on stderr so the user sees it.
                    eprintln!("winmux claude-hook: {}", e);
                    return ExitCode::from(3);
                }
            }
        }
        Cmd::SetupHooks {
            agent,
            dry_run,
            force,
        } => {
            let mut adapters: Vec<Box<dyn hooks::HookAdapter>> = Vec::new();
            match agent.as_str() {
                "claude" => adapters.push(Box::new(hooks::Claude)),
                "all" => {
                    adapters.push(Box::new(hooks::Claude));
                    adapters.push(Box::new(hooks::Stub { label: "Codex" }));
                    adapters.push(Box::new(hooks::Stub { label: "Cursor" }));
                    adapters.push(Box::new(hooks::Stub { label: "OpenCode" }));
                    adapters.push(Box::new(hooks::Stub { label: "Gemini CLI" }));
                    adapters.push(Box::new(hooks::Stub { label: "Copilot CLI" }));
                }
                other => {
                    eprintln!("error: unknown --agent {:?} (use 'claude' or 'all')", other);
                    return ExitCode::from(2);
                }
            }
            hooks::run_all(&adapters, *dry_run, *force);
            return ExitCode::SUCCESS;
        }
    };

    match result {
        Ok(v) => {
            if cli.quiet {
                ExitCode::SUCCESS
            } else {
                let s = if cli.raw {
                    serde_json::to_string(&v).unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(&v).unwrap_or_default()
                };
                println!("{}", s);
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(2)
        }
    }
}
