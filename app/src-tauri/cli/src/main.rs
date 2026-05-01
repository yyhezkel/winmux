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
        format!("{}…", &s[..280])
    } else {
        s
    }
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

#[tokio::main]
async fn main() -> ExitCode {
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
                                format!("{}…", &txt[..80])
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
            eprintln!(
                "claude-hook[{}] timeout={}s",
                subcommand, timeout_secs
            );

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
                    eprintln!("claude-hook[{}] decision={}", subcommand, decision);
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
                Err(e) => {
                    eprintln!("claude-hook error: {}", e);
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
