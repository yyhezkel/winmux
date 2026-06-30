//! Phase 6.3: bridge a remote-forwarded TCP channel to the local Named Pipe RPC server.
//! Phase 6.4: replace the plain-token preamble with an HMAC-SHA256 challenge-response
//! handshake so the shared secret never travels in cleartext.
//!
//! Phase 51.C: moved out of `app/src-tauri/src/tunnel.rs` into its own
//! crate. Depends on `winmux-core` for `dlog`, `pipe_name`, and the
//! `SshClient` type alias used in russh `Handle<SshClient>` signatures.

use hmac::{Hmac, Mac};
use rand::RngCore;
use russh::{Channel, ChannelMsg};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};

use winmux_core::{dlog, pipe_name, SshClient};

type HmacSha256 = Hmac<Sha256>;

const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

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

/// Perform the server-side half of the HMAC challenge-response.
/// Returns `Ok(())` if the client proved knowledge of the token; on failure, it has
/// already written `WINMUX-DENIED ...` to the stream and the caller should drop it.
async fn perform_handshake<S>(bs: &mut BufStream<S>, expected_token: &str) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // 1) Send challenge.
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    let challenge_line = format!("WINMUX-CHALLENGE {}\n", hex_encode(&nonce));
    bs.write_all(challenge_line.as_bytes())
        .await
        .map_err(|e| format!("write challenge: {e}"))?;
    bs.flush().await.map_err(|e| format!("flush: {e}"))?;

    // 2) Read response with a timeout — clients that never respond should be dropped.
    let mut line = String::new();
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        bs.read_line(&mut line),
    )
    .await;
    match read {
        Ok(Ok(0)) => return Err("client closed before sending response".into()),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("read response: {e}")),
        Err(_) => return Err("response timed out".into()),
    }
    let line = line.trim();
    let resp_hex = match line.strip_prefix("WINMUX-RESPONSE ") {
        Some(x) => x,
        None => {
            let _ = bs.write_all(b"WINMUX-DENIED bad-format\n").await;
            let _ = bs.flush().await;
            return Err(format!("bad response framing: {:?}", line));
        }
    };
    let resp = hex_decode(resp_hex)?;

    // 3) Verify HMAC in constant time (`Hmac::verify_slice`).
    let mut mac = HmacSha256::new_from_slice(expected_token.as_bytes())
        .map_err(|e| format!("hmac key: {e}"))?;
    mac.update(&nonce);
    if mac.verify_slice(&resp).is_err() {
        let _ = bs.write_all(b"WINMUX-DENIED bad-mac\n").await;
        let _ = bs.flush().await;
        return Err("hmac verify failed".into());
    }

    // 4) Tell the client we're good.
    bs.write_all(b"WINMUX-OK\n")
        .await
        .map_err(|e| format!("write OK: {e}"))?;
    bs.flush().await.map_err(|e| format!("flush OK: {e}"))?;
    Ok(())
}

pub async fn bridge_to_pipe(
    channel: Channel<russh::client::Msg>,
    expected_token: &str,
) -> Result<(), String> {
    let stream = channel.into_stream();
    let mut bs = BufStream::new(stream);

    if let Err(e) = perform_handshake(&mut bs, expected_token).await {
        dlog(&format!("tunnel: handshake REJECTED — {e}"));
        return Err(e);
    }
    dlog("tunnel: handshake OK");

    // Open a fresh client connection to the local pipe server.
    // Phase 39.A: on ERROR_PIPE_NOT_AVAILABLE (231) — all server
    // instances momentarily busy — retry with bounded exponential
    // backoff instead of failing the bridge. After the rpc_server cap
    // lift + parallel-accept fixes this path should be effectively
    // unreachable, but a remote agent that races a hair ahead of the
    // server no longer turns a transient busy into a hard error +
    // log spam. Per-attempt waits are silent (tracing::debug only);
    // a genuine give-up surfaces via spawn_bridge's dlog.
    let pipe_name = pipe_name();
    let mut backoff_ms = 25u64;
    let mut pipe = loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(231) && backoff_ms <= 800 => {
                tracing::debug!(
                    "tunnel: pipe busy (231), retrying in {}ms",
                    backoff_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms *= 2;
            }
            Err(e) => return Err(format!("open pipe {}: {}", pipe_name, e)),
        }
    };

    dlog("tunnel: bridging channel <-> pipe");
    bridge_copy(bs, pipe).await;
    Ok(())
}

/// v0.3.1 (pipe-leak belt-and-suspenders): copy each direction and finish as
/// soon as EITHER side reaches EOF, shutting down the peer's write so the other
/// end unblocks immediately. `copy_bidirectional` waits for BOTH directions to
/// close, which deadlocked here: the russh channel stream never surfaced the
/// remote CLI's close, so the pipe stayed open and its rpc_server instance
/// leaked (after 254, ERROR_PIPE_BUSY wedged every connection). Half-closing on
/// first-EOF frees the pipe instance the moment the one-shot RPC reply is done
/// — independent of the handler-side one-shot fix.
async fn bridge_copy<A, B>(a: A, b: B)
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    let a2b = async {
        let n = tokio::io::copy(&mut ar, &mut bw).await;
        let _ = bw.shutdown().await; // EOF → unblocks the peer's read
        n
    };
    let b2a = async {
        let n = tokio::io::copy(&mut br, &mut aw).await;
        let _ = aw.shutdown().await;
        n
    };
    tokio::select! {
        r = a2b => dlog(&format!("tunnel: bridge done (a→b: {r:?})")),
        r = b2a => dlog(&format!("tunnel: bridge done (b→a: {r:?})")),
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
    handle: &mut russh::client::Handle<SshClient>,
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

    exec_simple(handle, &format!("mkdir -p {}", env_dir)).await?;
    let write_cmd = format!(
        "cat > {} <<'__WINMUX_EOF__'\n{}__WINMUX_EOF__\nchmod 0600 {}",
        env_file, body, env_file
    );
    exec_simple(handle, &write_cmd).await?;
    dlog(&format!("tunnel: wrote {} ({} bytes)", env_file, body.len()));
    Ok(())
}

async fn exec_simple(
    handle: &mut russh::client::Handle<SshClient>,
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

#[cfg(test)]
mod tests {
    use super::bridge_copy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // v0.3.1 pipe-leak fix: when the rpc_server handler closes after sending
    // its one-shot reply, the bridge must release PROMPTLY (not hang waiting
    // for the channel side to also EOF, as copy_bidirectional did). Models the
    // bridge with two in-memory duplex pipes: `cli` <-> bridge <-> `server`.
    #[tokio::test]
    async fn bridge_releases_when_server_closes_after_one_reply() {
        let (mut cli, bridge_a) = tokio::io::duplex(1024);
        let (bridge_b, mut server) = tokio::io::duplex(1024);
        let bridge = tokio::spawn(async move { bridge_copy(bridge_a, bridge_b).await });

        // CLI sends one request.
        cli.write_all(b"REQ\n").await.unwrap();

        // Server (rpc_server handler) reads it, replies once, then CLOSES.
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"REQ\n");
        server.write_all(b"RESP\n").await.unwrap();
        drop(server); // one-shot handler returns → stream dropped

        // CLI receives the reply, then EOF (bridge shut its write down).
        let mut out = Vec::new();
        cli.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"RESP\n");

        // The bridge task must finish promptly — the whole point of the fix.
        tokio::time::timeout(std::time::Duration::from_secs(2), bridge)
            .await
            .expect("bridge_copy must release after the server closed")
            .unwrap();
    }

    // The mirror case: when the CLI side closes first (channel EOF), the bridge
    // must shut the pipe write down so the rpc_server handler's read sees EOF.
    #[tokio::test]
    async fn bridge_releases_when_client_closes_first() {
        let (cli, bridge_a) = tokio::io::duplex(1024);
        let (bridge_b, mut server) = tokio::io::duplex(1024);
        let bridge = tokio::spawn(async move { bridge_copy(bridge_a, bridge_b).await });

        drop(cli); // remote CLI hung up

        // The server side must observe EOF (read returns 0), not hang.
        let mut buf = [0u8; 16];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), server.read(&mut buf))
            .await
            .expect("server read must not hang after client close")
            .unwrap();
        assert_eq!(n, 0, "server should see EOF");
        tokio::time::timeout(std::time::Duration::from_secs(2), bridge)
            .await
            .expect("bridge must release after client close")
            .unwrap();
    }
}
