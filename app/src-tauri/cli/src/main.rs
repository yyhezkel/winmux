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

async fn rpc_call(method: &str, params: Value) -> Result<Value, String> {
    // Prefer TCP if WINMUX_SOCKET_ADDR is set (works on any OS, including remote tunnels).
    if let Ok(addr) = std::env::var("WINMUX_SOCKET_ADDR") {
        let stream = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("connect tcp {}: {}", addr, e))?;
        return rpc_via(stream, method, params).await;
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
        return rpc_via(pipe, method, params).await;
    }

    #[cfg(not(windows))]
    {
        Err("no transport configured: set WINMUX_SOCKET_ADDR=host:port".into())
    }
}

async fn rpc_via<S>(stream: S, method: &str, params: Value) -> Result<Value, String>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
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
    let mut buf = BufReader::new(reader);
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
