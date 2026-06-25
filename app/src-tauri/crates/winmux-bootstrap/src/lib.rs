//! Phase 6.2: bootstrap the winmux Linux binary on a remote SSH server.
//!
//! Best-effort. Called after auth succeeds, before opening the user's shell channel.
//! Detects the remote arch, hashes the existing binary (if any), and uploads via SFTP
//! when the hash doesn't match the manifest. Maintains a `~/.winmux/bin/winmux`
//! symlink to the architecture-specific binary.
//!
//! Phase 51.D: moved out of `app/src-tauri/src/remote_bootstrap.rs`. Per
//! Yossi's choice (option c): no `tauri` dep in this crate. The caller
//! (app) resolves Tauri resource paths and passes the manifest + a
//! resource-loader closure in. `bootstrap()` does all the russh+sftp
//! work without ever touching `AppHandle`.

use std::collections::HashMap;

use russh::client::Handle;
use russh::ChannelMsg;
use serde::Deserialize;

use winmux_core::{dlog, shell_quote, SshClient};

const REMOTE_DIR: &str = ".winmux/bin";
/// Phase tmux-conf: the per-arch-independent assets — currently just
/// `winmux-tmux.conf` — live at `~/.winmux/<file>` (sibling of `bin/`).
const REMOTE_BASE_DIR: &str = ".winmux";
const TMUX_CONF_REMOTE: &str = "tmux.conf";
const TMUX_CONF_MANIFEST_KEY: &str = "tmux-conf";

#[derive(Deserialize, Debug)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
    #[allow(dead_code)]
    pub size: u64,
    #[allow(dead_code)]
    pub built_at: String,
}

#[derive(Debug)]
pub enum BootstrapStatus {
    AlreadyOk,
    Uploaded {
        bytes: usize,
        #[allow(dead_code)]
        sha256: String,
    },
    UnsupportedArch(String),
}

/// Caller-provided helper to read a manifest-relative resource as
/// bytes. The caller knows where the resources live (Tauri's resource
/// bundling, dev filesystem layout, etc.); this crate just calls the
/// closure with the manifest entry's `path` field.
pub type ResourceLoader<'a> = &'a (dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync);

/// Parse a Tauri-bundled remote-manifest.json. Strips a UTF-8 BOM if
/// present (PowerShell 5.1 writes one and serde_json refuses to parse).
pub fn parse_manifest(text: &str) -> Result<HashMap<String, ManifestEntry>, String> {
    let stripped = text.trim_start_matches('\u{FEFF}');
    serde_json::from_str(stripped).map_err(|e| format!("parse manifest: {e}"))
}

async fn ssh_exec(
    handle: &mut Handle<SshClient>,
    cmd: &str,
) -> Result<(String, i32), String> {
    dlog(&format!("bootstrap: exec '{}'", cmd));
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open exec channel: {e}"))?;
    chan.exec(true, cmd).await.map_err(|e| format!("exec: {e}"))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExtendedData { data, ext: _ }) => {
                stderr.extend_from_slice(&data[..])
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let _ = chan.close().await;
    let stdout_str = String::from_utf8_lossy(&stdout).to_string();
    let stderr_str = String::from_utf8_lossy(&stderr).to_string();
    dlog(&format!(
        "bootstrap: exec '{}' exit={} stdout={:?} stderr={:?}",
        cmd,
        exit_code,
        stdout_str.trim(),
        stderr_str.trim()
    ));
    Ok((stdout_str, exit_code))
}

fn detect_triple(uname_output: &str) -> Option<&'static str> {
    let s = uname_output.trim();
    match s {
        "Linux x86_64" => Some("x86_64-linux"),
        "Linux aarch64" => Some("aarch64-linux"),
        _ => None,
    }
}

async fn upload_via_sftp(
    handle: &mut Handle<SshClient>,
    abs_remote_path: &str,
    bytes: &[u8],
    expected_hash: &str,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    // Phase 39.D: write to a sibling `.tmp` then atomically `mv -f` it onto the
    // final name. Truncating a currently-executing binary in place returns
    // ETXTBSY, which OpenSSH SFTP reports as the generic SSH_FX_FAILURE
    // ("Failure: Failure"); rename(2) instead swaps the directory entry to a
    // fresh inode, so a still-running old binary never blocks the replace.
    let tmp_path = format!("{abs_remote_path}.tmp");
    dlog(&format!(
        "remote bootstrap: uploading to {tmp_path} then atomic-rename to {abs_remote_path} (sha256 {expected_hash})"
    ));

    dlog(&format!(
        "bootstrap: opening sftp subsystem for {} ({} bytes)",
        tmp_path,
        bytes.len()
    ));
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: sftp channel_open failed: {e}"));
            format!("open sftp channel: {e}")
        })?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: sftp request_subsystem failed: {e}"));
            format!("request sftp: {e}")
        })?;
    let stream = chan.into_stream();
    let sftp = russh_sftp::client::SftpSession::new(stream)
        .await
        .map_err(|e| {
            dlog(&format!("bootstrap: SftpSession::new failed: {e}"));
            format!("sftp init: {e}")
        })?;
    dlog("bootstrap: sftp session ready");

    {
        let mut file = sftp
            .create(&tmp_path)
            .await
            .map_err(|e| {
                dlog(&format!("bootstrap: sftp.create {tmp_path} failed: {e}"));
                format!("sftp create {tmp_path}: {e}")
            })?;
        file.write_all(bytes)
            .await
            .map_err(|e| {
                dlog(&format!("bootstrap: sftp write_all failed: {e}"));
                format!("sftp write: {e}")
            })?;
        file.flush().await.ok();
        file.shutdown().await.ok();
    }
    dlog("bootstrap: sftp temp upload complete");

    let _ = sftp.close().await;

    // Atomic-replace the final path with the freshly-uploaded temp file.
    let mv_cmd = format!(
        "mv -f {} {}",
        shell_quote(&tmp_path),
        shell_quote(abs_remote_path)
    );
    let (_, mv_code) = ssh_exec(handle, &mv_cmd).await?;
    if mv_code != 0 {
        dlog(&format!(
            "bootstrap: atomic rename {tmp_path} -> {abs_remote_path} failed (exit {mv_code})"
        ));
        return Err(format!(
            "rename {tmp_path} -> {abs_remote_path}: exit {mv_code}"
        ));
    }
    dlog("bootstrap: sftp upload complete (atomic rename done)");

    Ok(())
}

pub async fn bootstrap(
    handle: &mut Handle<SshClient>,
    manifest: HashMap<String, ManifestEntry>,
    resource_loader: ResourceLoader<'_>,
    force: bool,
) -> Result<BootstrapStatus, String> {
    dlog(&format!("bootstrap: starting (force={force})"));

    // Identify remote.
    let (uname, code) = ssh_exec(handle, "uname -s -m").await?;
    if code != 0 {
        return Err(format!("uname failed: exit {code}"));
    }
    let triple = match detect_triple(&uname) {
        Some(t) => t,
        None => {
            dlog(&format!("bootstrap: unsupported arch '{}'", uname.trim()));
            return Ok(BootstrapStatus::UnsupportedArch(uname.trim().to_string()));
        }
    };
    dlog(&format!("bootstrap: triple = {}", triple));

    // Resolve manifest entry for this triple.
    let entry = manifest
        .get(triple)
        .ok_or_else(|| format!("no manifest entry for {triple}"))?;
    dlog(&format!(
        "bootstrap: manifest entry path={} sha256={}",
        entry.path, entry.sha256
    ));

    // Get remote $HOME so SFTP gets an absolute path.
    let (home_out, _) = ssh_exec(handle, "echo $HOME").await?;
    let home = home_out.trim();
    if home.is_empty() {
        return Err("empty $HOME on remote".into());
    }
    let remote_dir_abs = format!("{}/{}", home, REMOTE_DIR);
    let remote_bin_abs = format!("{}/{}", remote_dir_abs, entry.path);
    let remote_symlink_abs = format!("{}/winmux", remote_dir_abs);
    dlog(&format!(
        "bootstrap: remote paths — dir={} bin={} symlink={}",
        remote_dir_abs, remote_bin_abs, remote_symlink_abs
    ));

    // Compare existing hash unless forced.
    if !force {
        let (sum_out, _) = ssh_exec(
            handle,
            &format!("sha256sum {remote_bin_abs} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await?;
        let remote_hash = sum_out.trim().to_lowercase();
        if remote_hash == entry.sha256.to_lowercase() {
            dlog("bootstrap: hash matches existing — skipping upload");
            // Ensure symlink anyway.
            let _ = ssh_exec(
                handle,
                &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
            )
            .await;
            // Even when the binary is up to date, re-check the rc file
            // — the user may have wiped their shell config since the
            // last bootstrap, or this is a fresh machine that has the
            // binary cached but no PATH entry. Idempotent.
            ensure_path_in_rc(handle).await;
            return Ok(BootstrapStatus::AlreadyOk);
        }
        dlog(&format!(
            "bootstrap: hash mismatch — remote='{}' expected='{}' — will upload",
            remote_hash, entry.sha256
        ));
    }

    // Make dir, upload, chmod, symlink.
    ssh_exec(handle, &format!("mkdir -p {remote_dir_abs}")).await?;

    // Phase 39.D: reap any zombie port-watch from a prior session that may
    // still hold the binary's inode (e.g. orphaned by the pre-39.C pipe
    // crash). Non-fatal — pkill exits 1 when nothing matches, which is the
    // normal case; the trailing `true` keeps the channel exit clean.
    let _ = ssh_exec(handle, "pkill -f winmux-linux-x64 2>/dev/null; sleep 0.1; true").await;

    let bytes = resource_loader(&entry.path)?;
    upload_via_sftp(handle, &remote_bin_abs, &bytes, &entry.sha256).await?;
    ssh_exec(handle, &format!("chmod 0755 {remote_bin_abs}")).await?;
    ssh_exec(
        handle,
        &format!("ln -sf {remote_bin_abs} {remote_symlink_abs}"),
    )
    .await?;

    // Verify post-upload.
    let (verify_out, _) = ssh_exec(
        handle,
        &format!("sha256sum {remote_bin_abs} | awk '{{print $1}}'"),
    )
    .await?;
    let after_hash = verify_out.trim().to_lowercase();
    if after_hash != entry.sha256.to_lowercase() {
        dlog(&format!(
            "bootstrap: FAILED post-upload hash mismatch: got {} expected {}",
            after_hash, entry.sha256
        ));
        return Err(format!(
            "post-upload hash mismatch: got {after_hash}, expected {}",
            entry.sha256
        ));
    }
    dlog("bootstrap: COMPLETE — upload verified");

    // Phase 18: add `~/.winmux/bin` to the user's shell rc file so a
    // fresh non-winmux SSH session also gets `winmux` on PATH.
    ensure_path_in_rc(handle).await;

    // Phase tmux-conf: drop the bundled scrollback-friendly tmux
    // config at `~/.winmux/tmux.conf`. Whether tmux actually loads
    // it is decided per-pane at launch time (Settings →
    // `terminal.use_winmux_tmux_config`); we always upload so the
    // toggle works without re-bootstrapping.
    ensure_tmux_conf(handle, resource_loader, home, &manifest, force).await;

    Ok(BootstrapStatus::Uploaded {
        bytes: bytes.len(),
        sha256: entry.sha256.clone(),
    })
}

/// Phase tmux-conf: upload `winmux-tmux.conf` to `~/.winmux/tmux.conf`
/// if absent / hash drift / `force`. Best-effort — never fails the
/// bootstrap.
async fn ensure_tmux_conf(
    handle: &mut Handle<SshClient>,
    resource_loader: ResourceLoader<'_>,
    home: &str,
    manifest: &HashMap<String, ManifestEntry>,
    force: bool,
) {
    let entry = match manifest.get(TMUX_CONF_MANIFEST_KEY) {
        Some(e) => e,
        None => {
            dlog("bootstrap: tmux-conf entry missing from manifest — skipping upload");
            return;
        }
    };
    let remote_base = format!("{}/{}", home, REMOTE_BASE_DIR);
    let remote_conf = format!("{}/{}", remote_base, TMUX_CONF_REMOTE);

    if !force {
        let (sum_out, _) = match ssh_exec(
            handle,
            &format!("sha256sum {remote_conf} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                dlog(&format!("bootstrap: tmux-conf hash check failed: {e}"));
                return;
            }
        };
        if sum_out.trim().to_lowercase() == entry.sha256.to_lowercase() {
            dlog("bootstrap: tmux-conf hash matches — skipping upload");
            return;
        }
    }

    if let Err(e) = ssh_exec(handle, &format!("mkdir -p {remote_base}")).await {
        dlog(&format!("bootstrap: mkdir for tmux-conf failed: {e}"));
        return;
    }
    let bytes = match resource_loader(&entry.path) {
        Ok(b) => b,
        Err(e) => {
            dlog(&format!("bootstrap: read tmux-conf bundle failed: {e}"));
            return;
        }
    };
    if let Err(e) = upload_via_sftp(handle, &remote_conf, &bytes, &entry.sha256).await {
        dlog(&format!("bootstrap: upload tmux-conf failed: {e}"));
        return;
    }
    let _ = ssh_exec(handle, &format!("chmod 0644 {remote_conf}")).await;
    // Phase 65 (bug EE): the round-4 `tmux source-file` auto-apply was
    // removed. The conf now ships `mouse off`, and mouse-on is set
    // per-session via the new-session command chain (`\; set -g mouse on`,
    // see pane connect). Re-sourcing the conf into a running server would
    // reset `mouse` back to off globally and fight that injection on
    // every new pane. New sessions still pick up the conf via `-f`.
    dlog(&format!(
        "bootstrap: tmux-conf uploaded ({} bytes)",
        bytes.len()
    ));
}

/// Shell snippet that idempotently appends `~/.winmux/bin` to the
/// user's shell rc file. Shared between the bootstrap auto-fire
/// (best-effort, silent) and the Provisioning Wizard's
/// `AddWinmuxToPath` step (visible, ✓-in-the-log).
pub const PATH_RC_SNIPPET: &str = r#"
set -e
SH="$(basename "${SHELL:-/bin/bash}")"
case "$SH" in
  zsh)  RC="$HOME/.zshrc";    LINE='export PATH="$HOME/.winmux/bin:$PATH"' ;;
  fish) RC="$HOME/.config/fish/config.fish"; LINE='set -gx PATH $HOME/.winmux/bin $PATH' ;;
  *)    RC="$HOME/.bashrc";   LINE='export PATH="$HOME/.winmux/bin:$PATH"' ;;
esac
mkdir -p "$(dirname "$RC")" 2>/dev/null || true
touch "$RC" 2>/dev/null || { echo "ERROR cannot touch $RC"; exit 0; }
if grep -q 'winmux/bin' "$RC" 2>/dev/null; then
  echo "EXISTS $RC"
else
  printf '\n# Added by winmux — keep `winmux` on PATH\n%s\n' "$LINE" >> "$RC" || {
    echo "ERROR cannot write to $RC"; exit 0;
  }
  echo "ADDED $RC"
fi
"#;

async fn ensure_path_in_rc(handle: &mut Handle<SshClient>) {
    let result = ssh_exec(handle, PATH_RC_SNIPPET).await;
    match result {
        Ok((out, _exit)) => {
            let line = out.trim();
            if line.starts_with("ADDED ") {
                dlog(&format!(
                    "bootstrap: added PATH entry to {}",
                    line.trim_start_matches("ADDED ").trim()
                ));
            } else if line.starts_with("EXISTS ") {
                dlog(&format!(
                    "bootstrap: PATH already configured in {}",
                    line.trim_start_matches("EXISTS ").trim()
                ));
            } else {
                dlog(&format!("bootstrap: ensure_path_in_rc: {line}"));
            }
        }
        Err(e) => dlog(&format!("bootstrap: ensure_path_in_rc failed: {e}")),
    }
}
