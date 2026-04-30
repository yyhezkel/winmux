//! Phase 6.3: bridge a remote-forwarded TCP channel to the local Named Pipe RPC server.
//!
//! The Linux CLI on the remote host connects to `127.0.0.1:<remote_port>`. SSH wraps
//! that connection in a `forwarded-tcpip` channel which russh delivers to our handler.
//! The handler hands it to `bridge_to_pipe`, which validates a one-line auth token,
//! then bidirectionally copies the rest of the bytes between the SSH channel and a
//! fresh client connection to our named pipe RPC server.

use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncBufReadExt, BufStream};

use crate::dlog;

const TOKEN_PREAMBLE_MAX_LEN: usize = 128;

pub async fn bridge_to_pipe(
    channel: Channel<russh::client::Msg>,
    expected_token: &str,
) -> Result<(), String> {
    let stream = channel.into_stream();
    let mut bs = BufStream::new(stream);

    // First line on the channel must be `<token>\n`. If wrong, drop.
    let mut line = String::new();
    let read_res =
        tokio::time::timeout(std::time::Duration::from_secs(10), bs.read_line(&mut line)).await;
    match read_res {
        Ok(Ok(0)) => return Err("tunnel: client closed before sending token".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("tunnel: read token failed: {e}")),
        Err(_) => return Err("tunnel: token read timed out".into()),
    }
    if line.len() > TOKEN_PREAMBLE_MAX_LEN {
        return Err("tunnel: token preamble too long".into());
    }
    if line.trim() != expected_token {
        dlog("tunnel: REJECTED — token mismatch");
        return Err("tunnel: bad token".into());
    }

    // Open a fresh client connection to the local pipe server.
    let pipe_name = crate::rpc_server::pipe_name();
    let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(&pipe_name)
        .map_err(|e| format!("tunnel: open pipe {}: {}", pipe_name, e))?;
    let mut pipe = pipe;

    dlog("tunnel: token validated, bridging channel <-> pipe");
    // Bidirectional copy until either side closes.
    let r = tokio::io::copy_bidirectional(&mut bs, &mut pipe).await;
    match r {
        Ok((to_pipe, to_channel)) => {
            dlog(&format!(
                "tunnel: closed normally ({} bytes → pipe, {} bytes → channel)",
                to_pipe, to_channel
            ));
            Ok(())
        }
        Err(e) => {
            dlog(&format!("tunnel: copy_bidirectional ended: {e}"));
            // Most "connection reset" errors are normal session-end; not worth surfacing.
            Ok(())
        }
    }
}

/// Random alphanumeric token for the per-connection tunnel.
pub fn generate_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// Write the env file `~/.winmux/run/last.env` on the remote so the CLI can pick up
/// `WINMUX_SOCKET_ADDR` and `WINMUX_TUNNEL_TOKEN` even if sshd's `AcceptEnv` rejects
/// the per-channel `set_env` requests.
pub async fn write_remote_env_file(
    handle: &mut russh::client::Handle<crate::SshClient>,
    home: &str,
    socket_addr: &str,
    token: &str,
    pane_id: &str,
) -> Result<(), String> {
    let env_dir = format!("{}/.winmux/run", home);
    let env_file = format!("{}/last.env", env_dir);
    let body = format!(
        "WINMUX_SOCKET_ADDR={}\nWINMUX_TUNNEL_TOKEN={}\nWINMUX_PANE_ID={}\n",
        socket_addr, token, pane_id
    );

    // mkdir + write via ssh exec — simpler than another SFTP session.
    let mkdir_cmd = format!("mkdir -p {}", env_dir);
    exec_simple(handle, &mkdir_cmd).await?;

    // Use a heredoc so newlines/special chars are preserved verbatim.
    let write_cmd = format!(
        "cat > {} <<'__WINMUX_EOF__'\n{}__WINMUX_EOF__\nchmod 0600 {}",
        env_file, body, env_file
    );
    exec_simple(handle, &write_cmd).await?;
    dlog(&format!("tunnel: wrote {} ({} bytes)", env_file, body.len()));
    Ok(())
}

async fn exec_simple(
    handle: &mut russh::client::Handle<crate::SshClient>,
    cmd: &str,
) -> Result<(), String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open exec channel: {e}"))?;
    chan.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    if exit_code != 0 {
        return Err(format!("exec '{}' exit {}", cmd, exit_code));
    }
    Ok(())
}

/// Used as a small helper inside the russh `Handler`: spawn a bridge task. Exists so
/// the trait method body stays tiny.
pub fn spawn_bridge(channel: Channel<russh::client::Msg>, token: std::sync::Arc<String>) {
    tokio::spawn(async move {
        if let Err(e) = bridge_to_pipe(channel, &token).await {
            tracing::warn!("tunnel bridge: {e}");
            dlog(&format!("tunnel: bridge error: {e}"));
        }
    });
}
