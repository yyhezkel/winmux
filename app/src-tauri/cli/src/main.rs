// winmux CLI client.
//
// Transport selection:
// - On Windows, by default the CLI talks to the running winmux app over a per-user
//   named pipe at `\\.\pipe\winmux-<USER>`. Override with the `WINMUX_PIPE_PATH` env var.
// - On Linux/Unix (and as a Windows fallback when set), the CLI connects over TCP using
//   the address in `WINMUX_SOCKET_ADDR` (e.g. `127.0.0.1:8765`). This is the path used
//   when the binary runs on a remote SSH server tunneled back to a local listener.
// - If a Linux build can't find `WINMUX_SOCKET_ADDR`, it errors with exit code 2.

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
    /// Stub for Claude Code agent hooks: reads JSON from stdin, fires a notify.
    ClaudeHook {
        subcommand: String,
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
            rpc_call(
                "new-workspace",
                json!({
                    "name": name,
                    "connection": conn,
                    "cwd": cwd,
                    "color": color,
                }),
            )
            .await
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
        Cmd::ClaudeHook { subcommand } => {
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            eprintln!(
                "claude-hook[{}] received {} bytes from stdin",
                subcommand,
                buf.len()
            );
            if !buf.is_empty() {
                eprintln!("--- stdin payload ---");
                eprintln!("{}", buf.trim_end());
                eprintln!("---------------------");
            }
            let body = if buf.is_empty() {
                "(no stdin)".to_string()
            } else {
                buf.trim_end().to_string()
            };
            rpc_call(
                "notify",
                json!({
                    "title": format!("claude-hook: {}", subcommand),
                    "body": body,
                }),
            )
            .await
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
