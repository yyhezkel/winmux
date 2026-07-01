//! Phase 68.B — Add-on manager.
//!
//! Detects / installs / uninstalls / updates the winmux add-ons on a
//! workspace's remote over its existing SSH session. The manifest schema +
//! built-in registry live in the `winmux-addons` crate (68.A); this module
//! is the desktop side that runs the actions and exposes `addon_*` Tauri
//! commands for the Settings → Add-ons table + the wizards (68.E/F).
//!
//! Built-in routines are dispatched to the remote SHELL / remote CLI rather
//! than re-invoking the Rust bootstrap, so the connect-time bootstrap stays
//! the single owner of the CLI + tmux.conf upload (backward compatible —
//! those show up here as detect-only / "managed on connect"). Hooks are
//! fully manageable via the remote `winmux setup-hooks`, and `insights`
//! (68.C) ships a `winmux insights install` subcommand.

use std::sync::Arc;

use russh::client::Handle as SshHandle;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use tauri::State;
use tokio::io::AsyncWriteExt;

use winmux_addons::{
    builtin_registry, ids, manifest_for, routines, AddonAction, AddonManifest, AddonStatus,
};

use crate::{AppState, Session, SshClient};

// Phase 68.C / Phase 77: the cross-compiled server daemon (`winmux-server`,
// formerly `winmux-insights`), embedded so the AddonManager can SFTP-upload the
// arch-matched binary on install. On install we symlink the old
// `winmux-insights` name → `winmux-server` for backward compatibility.
const SERVER_X64: &[u8] = include_bytes!("../resources/winmux-server-linux-x64");
const SERVER_ARM64: &[u8] = include_bytes!("../resources/winmux-server-linux-arm64");

/// A live SSH handle for the workspace (mirrors file_manager's picker).
pub(crate) fn pick_handle(state: &AppState, workspace_id: &str) -> Option<Arc<SshHandle<SshClient>>> {
    let sessions = state.core.sessions.lock().ok()?;
    for sess in sessions.values() {
        if let Session::Ssh(s) = sess {
            if s.workspace_id == workspace_id {
                return Some(s.handle.clone());
            }
        }
    }
    None
}

/// Run a command on the remote and capture stdout+stderr (best-effort, timed).
pub(crate) async fn exec(handle: &SshHandle<SshClient>, cmd: &str, timeout_secs: u64) -> Result<String, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => out.extend_from_slice(data),
                ChannelMsg::ExtendedData { ref data, .. } => out.extend_from_slice(data),
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    Ok(String::from_utf8_lossy(&out).to_string())
}

pub(crate) async fn remote_home(handle: &SshHandle<SshClient>) -> String {
    exec(handle, "printf %s \"$HOME\"", 8)
        .await
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn expand(script: &str, home: &str) -> String {
    script
        .replace("${WINMUX_BIN}", &format!("{home}/.winmux/bin/winmux"))
        .replace("${REMOTE_HOME}", home)
}

/// Run an AddonAction → stdout (detect) or status text (install/etc).
async fn run_action(
    action: &AddonAction,
    handle: &SshHandle<SshClient>,
    home: &str,
) -> Result<String, String> {
    match action {
        AddonAction::Shell { script } => exec(handle, &expand(script, home), 90).await,
        AddonAction::Builtin { routine } => run_builtin(routine, handle, home).await,
        AddonAction::Noop => Ok(String::new()),
    }
}

/// Dispatch the built-in routines. cli/tmux-conf are connect-managed
/// (detect works; install just informs). hooks are fully managed via the
/// remote CLI's `setup-hooks`.
async fn run_builtin(
    routine: &str,
    handle: &SshHandle<SshClient>,
    home: &str,
) -> Result<String, String> {
    let bin = format!("{home}/.winmux/bin/winmux");
    match routine {
        routines::CLI_DETECT => {
            exec(handle, &format!("\"{bin}\" --version 2>/dev/null | head -1"), 8).await
        }
        routines::CLI_INSTALL => Ok("managed automatically on connect".into()),
        routines::TMUX_CONF_DETECT => {
            exec(
                handle,
                &format!("test -f \"{home}/.winmux/tmux.conf\" && echo present || true"),
                8,
            )
            .await
        }
        routines::TMUX_CONF_INSTALL => Ok("managed automatically on connect".into()),
        routines::HOOKS_DETECT => {
            // Pull winmux_meta.hooks_version out of settings.json if present.
            exec(
                handle,
                &format!(
                    "grep -o '\"hooks_version\"[^,}}]*' \"{home}/.claude/settings.json\" \
                     2>/dev/null | grep -o '[0-9][0-9.]*' | head -1 || true"
                ),
                8,
            )
            .await
        }
        routines::HOOKS_INSTALL => {
            exec(
                handle,
                &format!(
                    "\"{bin}\" setup-hooks --agent claude --source bundled --force 2>&1 | tail -1"
                ),
                30,
            )
            .await
        }
        routines::HOOKS_UNINSTALL => exec(handle, &hooks_uninstall_script(home), 15).await,
        routines::INSIGHTS_DETECT => {
            // Phase 77: prefer winmux-server; fall back to the legacy
            // winmux-insights name (symlink, or a pre-2.x install) so an
            // existing install is still detected during the upgrade window.
            exec(
                handle,
                &format!(
                    "( \"{home}/.winmux/bin/winmux-server\" --version 2>/dev/null || \
                       \"{home}/.winmux/bin/winmux-insights\" --version 2>/dev/null ) | head -1"
                ),
                8,
            )
            .await
        }
        routines::INSIGHTS_INSTALL => insights_install(handle, home).await,
        routines::INSIGHTS_UNINSTALL => {
            // Stop the service (any launch shape), drop the unit file so a stale
            // one can't relaunch it, remove the binary, then VERIFY the binary
            // is gone — a leftover would make `detect` still report installed,
            // hiding the Install button so the user can't reinstall.
            let out = exec(
                handle,
                &format!(
                    // Phase 77: tear down BOTH the new winmux-server and the legacy
                    // winmux-insights (unit, binary, symlink) so an upgraded or a
                    // pre-2.x install both uninstall cleanly.
                    "systemctl --user disable --now winmux-server 2>/dev/null; \
                     systemctl --user disable --now winmux-insights 2>/dev/null; \
                     pkill -x winmux-server 2>/dev/null; pkill -f 'winmux-server serve' 2>/dev/null; \
                     pkill -x winmux-insights 2>/dev/null; pkill -f 'winmux-insights serve' 2>/dev/null; \
                     rm -f \"{home}/.winmux/bin/winmux-server\" \"{home}/.winmux/bin/winmux-insights\"; \
                     rm -f \"$HOME/.config/systemd/user/winmux-server.service\" \
                           \"$HOME/.config/systemd/user/winmux-insights.service\"; \
                     systemctl --user daemon-reload 2>/dev/null; \
                     if [ -e \"{home}/.winmux/bin/winmux-server\" ] || [ -e \"{home}/.winmux/bin/winmux-insights\" ]; \
                       then echo STILL_PRESENT; else echo removed; fi"
                ),
                15,
            )
            .await;
            if let Ok(o) = &out {
                if o.contains("STILL_PRESENT") {
                    crate::dlog_tag("ADDON", "insights uninstall — binary STILL PRESENT after rm");
                    return Err(
                        "could not remove the daemon binary on the server (still present after rm)"
                            .into(),
                    );
                }
            }
            out
        }
        routines::NGINX_PROXY_DETECT => nginx_proxy_detect(handle).await,
        routines::NGINX_PROXY_INSTALL => Err(
            "Mobile Proxy needs a domain + Cloudflare token — install it from \
             Monitor → Mobile."
                .into(),
        ),
        routines::NGINX_PROXY_UNINSTALL => nginx_proxy_uninstall(handle).await,
        other => Err(format!("unknown builtin routine {other}")),
    }
}

/// Upload bytes to a remote path via a fresh SFTP channel (atomic-ish:
/// write to .tmp then mv).
async fn sftp_upload(
    handle: &SshHandle<SshClient>,
    remote_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let chan = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("open channel: {e}"))?;
    chan.request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("request sftp: {e}"))?;
    let sftp = SftpSession::new(chan.into_stream())
        .await
        .map_err(|e| format!("sftp init: {e}"))?;
    {
        let mut f = sftp
            .create(remote_path)
            .await
            .map_err(|e| format!("sftp create {remote_path}: {e}"))?;
        f.write_all(bytes)
            .await
            .map_err(|e| format!("sftp write: {e}"))?;
        f.flush().await.ok();
        f.shutdown().await.ok();
    }
    let _ = sftp.close().await;
    Ok(())
}

// ─── Phase 70.A: nginx reverse proxy + Let's Encrypt (Cloudflare DNS) ─────

/// Run a command on the remote feeding `stdin` to it, capturing stdout+stderr.
/// Used to pass secrets (the Cloudflare token) over the encrypted SSH channel
/// instead of on the command line (never ps-visible, never in the cmd string).
pub(crate) async fn exec_stdin(
    handle: &SshHandle<SshClient>,
    cmd: &str,
    stdin: &[u8],
    timeout_secs: u64,
) -> Result<String, String> {
    let mut ch = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("channel_open: {e}"))?;
    ch.exec(true, cmd.as_bytes())
        .await
        .map_err(|e| format!("exec: {e}"))?;
    ch.data(stdin).await.map_err(|e| format!("stdin: {e}"))?;
    ch.eof().await.ok();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => out.extend_from_slice(data),
                ChannelMsg::ExtendedData { ref data, .. } => out.extend_from_slice(data),
                ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    Ok(String::from_utf8_lossy(&out).to_string())
}

/// §3.1: resolve how to run a privileged command. Returns the prefix to put
/// before the command: "" when already root, "sudo " when passwordless sudo
/// works, otherwise a clean error (interactive sudo password is a follow-up).
async fn resolve_privilege(handle: &SshHandle<SshClient>) -> Result<String, String> {
    let uid = exec(handle, "id -u", 8).await.unwrap_or_default();
    if uid.trim() == "0" {
        return Ok(String::new());
    }
    let probe = exec(handle, "sudo -n true 2>&1 && echo WINMUX_SUDO_OK", 8)
        .await
        .unwrap_or_default();
    if probe.contains("WINMUX_SUDO_OK") {
        return Ok("sudo ".into());
    }
    Err("this server's user is not root and passwordless sudo isn't available. \
         Connect the workspace as root, or enable NOPASSWD sudo for the user, \
         then retry."
        .into())
}

/// Strict domain validation before the value ever reaches a shell arg.
/// Lowercase letters/digits/hyphens per label, dot-separated, 1–253 chars.
fn valid_domain(d: &str) -> bool {
    let d = d.trim();
    if d.is_empty() || d.len() > 253 || d.starts_with('.') || d.ends_with('.') {
        return false;
    }
    d.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }) && d.contains('.')
}

/// The fixed installer script. NO untrusted interpolation: the domain arrives
/// as $1 (validated desktop-side) and the Cloudflare token on stdin (so it's
/// never on argv / in the command string). Idempotent: skips certbot if a
/// live cert already exists, overwrites the site config cleanly.
const NGINX_INSTALL_SCRIPT: &str = r#"#!/bin/bash
set -uo pipefail
DOMAIN="$1"
USERHOME="${2:-$HOME}"
# Cloudflare token comes on stdin (never argv / never logged).
IFS= read -r CF_TOKEN || true

# Unified per-service log: everything below is tee'd to ~/.winmux/logs so the
# desktop can read WHY an install failed instead of a black box. The token is
# never echoed, so it never lands here.
LOGDIR="$USERHOME/.winmux/logs"
mkdir -p "$LOGDIR" 2>/dev/null || true
LOG="$LOGDIR/mobile-install.log"
exec > >(tee -a "$LOG") 2>&1
echo "──────────────────────────────────────────────"
echo "[mobile] install start domain=$DOMAIN uid=$(id -u)"
fail() { echo "[mobile] FAILED: $1"; exit "${2:-1}"; }

[ -n "$CF_TOKEN" ] || fail "empty Cloudflare token" 2

export DEBIAN_FRONTEND=noninteractive
echo "[mobile] apt: installing nginx + certbot + dns-cloudflare (first run can take a minute)…"
apt-get update -qq || fail "apt-get update failed (is the user root / NOPASSWD sudo?)" 10
apt-get install -y -qq nginx certbot python3-certbot-dns-cloudflare || fail "apt install failed (see lines above)" 11

install -d -m 700 /etc/winmux
umask 177
printf 'dns_cloudflare_api_token = %s\n' "$CF_TOKEN" > /etc/winmux/cloudflare.ini
chmod 600 /etc/winmux/cloudflare.ini
echo "[mobile] wrote /etc/winmux/cloudflare.ini (600)"

if [ ! -d "/etc/letsencrypt/live/$DOMAIN" ]; then
  echo "[mobile] certbot: requesting Let's Encrypt cert for $DOMAIN via Cloudflare DNS-01…"
  certbot certonly --dns-cloudflare \
    --dns-cloudflare-credentials /etc/winmux/cloudflare.ini \
    --dns-cloudflare-propagation-seconds 30 \
    -d "$DOMAIN" --non-interactive --agree-tos \
    --register-unsafely-without-email \
    || fail "certbot failed — check the domain is on Cloudflare and the token has Zone:DNS:Edit + Zone:Zone:Read" 3
else
  echo "[mobile] certbot: existing cert for $DOMAIN reused"
fi

# limit_req_zone is an http-context directive — it MUST live outside any
# server{} block. Define it once in conf.d (loaded once at http level, so it
# is shared across every winmux vhost and can't collide as "already bound").
cat > /etc/nginx/conf.d/winmux-ratelimit.conf <<'RL'
limit_req_zone $binary_remote_addr zone=winmux:10m rate=20r/s;
RL

SITE="/etc/nginx/sites-available/winmux-$DOMAIN"
cat > "$SITE" <<NGINX
server {
  listen 443 ssl http2;
  server_name $DOMAIN;
  ssl_certificate /etc/letsencrypt/live/$DOMAIN/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/$DOMAIN/privkey.pem;
  ssl_protocols TLSv1.2 TLSv1.3;
  add_header Strict-Transport-Security "max-age=31536000" always;
  location / {
    limit_req zone=winmux burst=40 nodelay;
    proxy_pass http://127.0.0.1:7879;
    proxy_http_version 1.1;
    proxy_set_header Upgrade \$http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host \$host;
    proxy_set_header X-Real-IP \$remote_addr;
    proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto https;
    proxy_read_timeout 86400;
  }
}
NGINX
ln -sf "$SITE" "/etc/nginx/sites-enabled/winmux-$DOMAIN"
nginx -t || fail "nginx config test failed (nginx -t above)" 4
systemctl reload nginx || systemctl restart nginx || fail "nginx reload/restart failed" 5
echo "[mobile] nginx reloaded — proxy live on 443 → 127.0.0.1:7879"

mkdir -p /etc/letsencrypt/renewal-hooks/post
printf '#!/bin/bash\nsystemctl reload nginx\n' > /etc/letsencrypt/renewal-hooks/post/winmux-reload-nginx.sh
chmod +x /etc/letsencrypt/renewal-hooks/post/winmux-reload-nginx.sh

# Hand the log back to the (non-root) SSH user so the desktop can tail it.
chown "$(stat -c %u:%g "$USERHOME" 2>/dev/null || echo 0:0)" "$LOG" 2>/dev/null || true
echo "WINMUX_NGINX_OK $DOMAIN"
"#;

/// Param-driven install (called by `mobile_pairing_init`, 70.D). Validates the
/// domain, resolves privilege (§3.1), uploads the fixed script, and runs it
/// with the CF token piped on stdin. Returns the script's last status line.
pub(crate) async fn nginx_proxy_install(
    handle: &SshHandle<SshClient>,
    home: &str,
    domain: &str,
    cf_token: &str,
) -> Result<String, String> {
    if !valid_domain(domain) {
        return Err("invalid domain".into());
    }
    crate::dlog_tag("MOBILE", &format!("nginx install begin domain={domain}"));
    let prefix = resolve_privilege(handle).await?;
    let script_path = format!("{home}/.winmux/run/nginx-install.sh");
    let _ = exec(handle, &format!("mkdir -p \"{home}/.winmux/run\""), 8).await;
    sftp_upload(handle, &script_path, NGINX_INSTALL_SCRIPT.as_bytes()).await?;
    // domain + home are validated/trusted → safe as quoted args; token is on
    // stdin only. 300s: a first-time apt install of certbot + the DNS plugin
    // plus the DNS-01 propagation wait can legitimately exceed 3 minutes.
    let cmd = format!("{prefix}bash \"{script_path}\" \"{domain}\" \"{home}\"");
    let out = exec_stdin(handle, &cmd, cf_token.as_bytes(), 300).await?;
    let _ = exec(handle, &format!("rm -f \"{script_path}\""), 8).await;

    // The script tees its whole transcript to us; log the tail (Rule #1/#8 safe
    // — the token is never echoed) so debug.log explains a failure, and point
    // at the persistent server-side log for the full detail.
    let tail: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in tail.iter().rev().take(12).rev() {
        crate::dlog_tag("MOBILE", l);
    }
    let log_path = format!("{home}/.winmux/logs/mobile-install.log");
    if out.contains("WINMUX_NGINX_OK") {
        crate::dlog_tag("MOBILE", &format!("nginx install OK domain={domain}"));
        Ok(format!("nginx + TLS ready for {domain}"))
    } else {
        // Prefer the explicit "[mobile] FAILED: …" line the script emits.
        let reason = tail
            .iter()
            .rev()
            .find(|l| l.contains("FAILED:"))
            .and_then(|l| l.split("FAILED:").nth(1))
            .map(str::trim)
            .or_else(|| tail.last().copied())
            .unwrap_or("(no output — likely a timeout during apt/certbot)");
        crate::dlog_tag("MOBILE", &format!("nginx install FAILED domain={domain}: {reason}"));
        Err(format!(
            "nginx install failed: {reason}. Full log on the server: {log_path}"
        ))
    }
}

/// detect: prints the add-on version if nginx is active (non-empty ⇒ installed
/// for the add-on framework), else empty.
async fn nginx_proxy_detect(handle: &SshHandle<SshClient>) -> Result<String, String> {
    let active = exec(handle, "systemctl is-active nginx 2>/dev/null || true", 8).await?;
    if active.trim() == "active" {
        Ok(winmux_addons::NGINX_PROXY_VERSION.to_string())
    } else {
        Ok(String::new())
    }
}

/// uninstall: disable+remove winmux nginx sites and the CF credential. Leaves
/// nginx itself installed (other services may use it).
async fn nginx_proxy_uninstall(handle: &SshHandle<SshClient>) -> Result<String, String> {
    let prefix = resolve_privilege(handle).await?;
    let script = format!(
        "set -e; \
         rm -f /etc/nginx/sites-enabled/winmux-* /etc/nginx/sites-available/winmux-*; \
         rm -f /etc/nginx/conf.d/winmux-ratelimit.conf; \
         rm -f /etc/winmux/cloudflare.ini; \
         (nginx -t >/dev/null 2>&1 && systemctl reload nginx) || true; \
         echo removed"
    );
    exec(handle, &format!("{prefix}bash -c '{script}'"), 30).await
}

/// 68.C: install the insights daemon — arch-detect, SFTP-upload the
/// embedded binary, then start it as a `systemd --user` service (falling
/// back to nohup). The daemon self-creates its API token on first run.
async fn insights_install(handle: &SshHandle<SshClient>, home: &str) -> Result<String, String> {
    let uname = exec(handle, "uname -m", 8).await.unwrap_or_default();
    let bytes: &[u8] = if uname.contains("aarch64") || uname.contains("arm64") {
        SERVER_ARM64
    } else {
        SERVER_X64
    };
    let _ = exec(
        handle,
        &format!("mkdir -p \"{home}/.winmux/bin\" \"{home}/.winmux/server\""),
        8,
    )
    .await;
    // Phase 77: install as `winmux-server`; keep a `winmux-insights` symlink so
    // anything referencing the old name still resolves. Data dir is
    // ~/.winmux/server (the 2.0 binary's default); on an in-place 1.x→2.x upgrade
    // the daemon migrates ~/.winmux/insights → ~/.winmux/server on first boot,
    // preserving the token + chat.db + paired_devices (paired phones keep working).
    let final_path = format!("{home}/.winmux/bin/winmux-server");
    let legacy_link = format!("{home}/.winmux/bin/winmux-insights");
    let tmp = format!("{final_path}.tmp");
    sftp_upload(handle, &tmp, bytes).await?;
    let _ = exec(
        handle,
        &format!(
            "chmod 0755 \"{tmp}\" && mv -f \"{tmp}\" \"{final_path}\" && \
             ln -sf \"{final_path}\" \"{legacy_link}\""
        ),
        10,
    )
    .await;
    // Phase 72.1: one script that (a) ensures the daemon user can reach Docker
    // and (b) starts the daemon so it actually HAS that access. The daemon
    // runs as this user, but a systemd --user service does NOT inherit the
    // login session's supplementary groups — so even a user already in the
    // `docker` group gets EACCES on the socket. Fix: ensure group membership
    // (usermod via passwordless sudo if needed), then launch the daemon under
    // `sg docker`, which reads /etc/group directly and grants the group
    // immediately — NO reconnect required. Best-effort; never fails install.
    let start = format!(
        r#"DAEMON="{final_path}"
U=$(id -un)
NOTE="no-docker"
WRAP=""
if command -v docker >/dev/null 2>&1; then
  if [ -n "$XDG_RUNTIME_DIR" ] && [ -S "$XDG_RUNTIME_DIR/docker.sock" ]; then
    NOTE="rootless"
  elif getent group docker >/dev/null 2>&1; then
    if id -nG "$U" 2>/dev/null | grep -qw docker; then
      NOTE="member"
    elif command -v sudo >/dev/null 2>&1 && sudo -n usermod -aG docker "$U" 2>/dev/null; then
      NOTE="added"
    else
      NOTE="need-sudo"
    fi
    if id -nG "$U" 2>/dev/null | grep -qw docker && command -v sg >/dev/null 2>&1; then
      WRAP="sg"
    fi
  else
    NOTE="no-group"
  fi
fi
SG=$(command -v sg 2>/dev/null)
if [ "$WRAP" = "sg" ] && [ -n "$SG" ]; then
  EXECSTART="$SG docker -c \"exec $DAEMON serve\""
else
  EXECSTART="$DAEMON serve"
fi
mkdir -p "$HOME/.config/systemd/user"
# Phase 77: retire the old winmux-insights unit if present (the daemon is now
# winmux-server); its data dir is unchanged so nothing is lost.
systemctl --user disable --now winmux-insights >/dev/null 2>&1
rm -f "$HOME/.config/systemd/user/winmux-insights.service"
cat > "$HOME/.config/systemd/user/winmux-server.service" <<UNIT
[Unit]
Description=winmux server daemon
After=network.target
[Service]
ExecStart=$EXECSTART
Restart=on-failure
RestartSec=5
[Install]
WantedBy=default.target
UNIT
if command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload 2>/dev/null; then
  systemctl --user enable winmux-server >/dev/null 2>&1
  systemctl --user restart winmux-server >/dev/null 2>&1 && echo "started (systemd --user)"
else
  pkill -x winmux-server 2>/dev/null; pkill -f 'winmux-server serve' 2>/dev/null
  pkill -x winmux-insights 2>/dev/null; pkill -f 'winmux-insights serve' 2>/dev/null
  sleep 1
  if [ "$WRAP" = "sg" ] && [ -n "$SG" ]; then
    nohup "$SG" docker -c "exec $DAEMON serve" >/dev/null 2>&1 &
  else
    nohup "$DAEMON" serve >/dev/null 2>&1 &
  fi
  echo "started (nohup)"
fi
echo "WINMUX_DOCKER=$NOTE"
"#
    );
    let r = exec(handle, &start, 25).await?;
    let started = r
        .lines()
        .find(|l| l.trim_start().starts_with("started ("))
        .map(|l| l.trim().to_string())
        .unwrap_or_default();
    let marker = r
        .lines()
        .find_map(|l| l.trim().strip_prefix("WINMUX_DOCKER="))
        .unwrap_or("no-docker");
    Ok(format!("installed; {started}{}", docker_group_message(marker)))
}

/// Map a WINMUX_DOCKER=<marker> to the human suffix appended to the install
/// status. Pure (unit-tested).
fn docker_group_message(marker: &str) -> String {
    match marker {
        // member/added: the daemon is (re)started under `sg docker`, so it has
        // the group right now — no reconnect needed.
        "member" => " · Docker group OK (daemon runs under sg docker)".to_string(),
        "added" => " · added user to the 'docker' group and (re)started the daemon under it \
                     — Docker monitoring should work now"
            .to_string(),
        "rootless" => " · rootless Docker detected (no group needed)".to_string(),
        "need-sudo" => " · Docker needs a one-time manual step: run `sudo usermod -aG docker \
                        $USER` on the server, then reinstall this add-on"
            .to_string(),
        "no-group" => " · no 'docker' group (rootless Docker?)".to_string(),
        _ => String::new(), // no-docker → say nothing
    }
}

#[cfg(test)]
mod docker_group_tests {
    use super::docker_group_message;

    #[test]
    fn maps_markers_to_guidance() {
        assert!(docker_group_message("member").contains("sg docker"));
        assert!(docker_group_message("rootless").contains("rootless"));
        assert!(docker_group_message("added").contains("work now"));
        assert!(docker_group_message("need-sudo").contains("usermod -aG docker"));
        assert!(docker_group_message("no-group").contains("docker"));
        // no-docker (and anything unknown) → silent.
        assert_eq!(docker_group_message("no-docker"), "");
        assert_eq!(docker_group_message("weird"), "");
    }
}

/// Strip winmux hook entries from settings.json + hooks.json (best-effort).
fn hooks_uninstall_script(home: &str) -> String {
    format!(
        r#"python3 - <<'PY' 2>/dev/null || true
import json, os
for fn in ("settings.json", "hooks.json"):
    p = "{home}/.claude/" + fn
    if not os.path.exists(p): continue
    try: d = json.load(open(p))
    except Exception: continue
    blocks = d.get("hooks", d) if fn == "settings.json" else d
    for ev in list(blocks):
        if isinstance(blocks[ev], list):
            blocks[ev] = [e for e in blocks[ev] if "winmux" not in json.dumps(e)]
            if not blocks[ev]: del blocks[ev]
    d.pop("winmux_meta", None)
    json.dump(d, open(p, "w"), indent=2)
print("removed")
PY"#
    )
}

/// Detect → AddonStatus for one manifest.
async fn status_for(m: &AddonManifest, handle: &SshHandle<SshClient>, home: &str) -> AddonStatus {
    let detected = run_action(&m.detect, handle, home).await.unwrap_or_default();
    let v = detected.trim();
    let installed_version = if v.is_empty() {
        None
    } else {
        // Last whitespace token is usually the version (e.g. "winmux 0.2.8").
        Some(v.split_whitespace().last().unwrap_or(v).to_string())
    };
    let installed = installed_version.is_some();
    let update_available = installed_version
        .as_deref()
        .map(|iv| iv != m.version)
        .unwrap_or(false);
    crate::dlog_tag(
        "ADDON",
        &format!(
            "detect id={} → installed={installed} version={}",
            m.id,
            installed_version.as_deref().unwrap_or("-")
        ),
    );
    AddonStatus {
        id: m.id.clone(),
        installed,
        installed_version,
        available_version: m.version.clone(),
        update_available,
        busy: false,
        last_error: None,
    }
}

#[tauri::command]
pub(crate) async fn addon_list(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<AddonStatus>, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    if home.is_empty() {
        return Err("could not resolve remote $HOME".into());
    }
    let mut out = Vec::new();
    for m in builtin_registry() {
        out.push(status_for(&m, &handle, &home).await);
    }
    Ok(out)
}

async fn run_lifecycle(
    state: &AppState,
    workspace_id: &str,
    id: &str,
    op: &str,
    pick: impl Fn(&AddonManifest) -> AddonAction,
) -> Result<AddonStatus, String> {
    crate::dlog_tag("ADDON", &format!("{op} id={id} — begin"));
    let m = manifest_for(id).ok_or_else(|| format!("unknown add-on {id}"))?;
    let handle =
        pick_handle(state, workspace_id).ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    if home.is_empty() {
        return Err("could not resolve remote $HOME".into());
    }
    // Dependency resolution (install): deps are currently just winmux-cli,
    // which is always present in a connected session — so no extra work for
    // round 1. (A topological install of arbitrary deps lands with the
    // community-add-ons work.)
    let action = pick(&m);
    if let Err(e) = run_action(&action, &handle, &home).await {
        crate::dlog_tag("ADDON", &format!("{op} id={id} — action FAILED: {e}"));
        let mut s = status_for(&m, &handle, &home).await;
        s.last_error = Some(e);
        return Ok(s);
    }
    let s = status_for(&m, &handle, &home).await;
    crate::dlog_tag("ADDON", &format!("{op} id={id} — done (installed={})", s.installed));
    Ok(s)
}

#[tauri::command]
pub(crate) async fn addon_install(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, "install", |m| m.install.clone()).await
}

#[tauri::command]
pub(crate) async fn addon_uninstall(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, "uninstall", |m| m.uninstall.clone()).await
}

#[tauri::command]
pub(crate) async fn addon_update(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, "update", |m| m.update.clone()).await
}

// ─── Phase 68.D: Monitor — pull from the insights daemon over the tunnel ──
// The daemon binds 127.0.0.1:7879 on the remote; we reach it by curling it
// over the workspace's SSH session (no extra port-forward needed). The token
// is read from the daemon's own file on the remote, so it never transits the
// desktop.

/// Whitelist the API path (defends the interpolated curl URL).
fn safe_api_path(p: &str) -> bool {
    !p.is_empty()
        && p.starts_with('/')
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-?=&.,".contains(&b))
}

#[tauri::command]
pub(crate) async fn insights_fetch(
    state: State<'_, AppState>,
    workspace_id: String,
    path: String,
) -> Result<String, String> {
    if !safe_api_path(&path) {
        return Err("invalid insights path".into());
    }
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    // Phase 72.2: append the HTTP status via curl -w so we can tell "daemon
    // unreachable" (000 / no curl) apart from "auth failed" (401) apart from a
    // real body (200). We strip the marker before returning so the JSON stays
    // clean, and dlog the outcome (Rule #1: status + length only, never body).
    let cmd = format!(
        "curl -s --max-time 6 \
         -H \"Authorization: Bearer $(cat '{home}/.winmux/server/token' 2>/dev/null)\" \
         -w '\\nWINMUX_HTTP=%{{http_code}}' \
         'http://127.0.0.1:7879{path}' 2>/dev/null; \
         command -v curl >/dev/null 2>&1 || echo 'WINMUX_HTTP=nocurl'"
    );
    let raw = exec(&handle, &cmd, 10).await?;
    let status = raw
        .rsplit("WINMUX_HTTP=")
        .next()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // Body = everything before the last marker line.
    let body = match raw.rfind("WINMUX_HTTP=") {
        Some(i) => raw[..i].trim_end_matches('\n').to_string(),
        None => raw.clone(),
    };
    crate::dlog_tag(
        "MONITOR",
        &format!("fetch path={path} http={status} body_len={}", body.len()),
    );
    match status.as_str() {
        "200" => Ok(body),
        "nocurl" => Err("curl is not installed on this server (needed to reach the daemon)".into()),
        "401" | "403" => Err("insights daemon rejected the token — reinstall the add-on".into()),
        "000" | "" => Err("insights daemon not reachable on 127.0.0.1:7879 (is it running?)".into()),
        other => Err(format!("insights daemon returned HTTP {other}")),
    }
}

#[tauri::command]
pub(crate) async fn insights_docker_action(
    state: State<'_, AppState>,
    workspace_id: String,
    container_id: String,
    action: String,
) -> Result<String, String> {
    if !matches!(action.as_str(), "start" | "stop" | "restart" | "kill") {
        return Err("invalid docker action".into());
    }
    if container_id.is_empty() || !container_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("invalid container id".into());
    }
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let cmd = format!(
        "curl -s --max-time 8 -X POST -H 'Content-Type: application/json' \
         -H \"Authorization: Bearer $(cat '{home}/.winmux/server/token' 2>/dev/null)\" \
         -d '{{\"cmd\":\"{action}\"}}' \
         'http://127.0.0.1:7879/docker/{container_id}/action'"
    );
    exec(&handle, &cmd, 12).await
}

/// Phase 76: ask the daemon to SIGTERM the given PIDs (duplicate port-watchers
/// / orphan claude sessions). The daemon only kills PIDs it itself classifies
/// as killable, so a bad PID list is a safe no-op. `pids` are validated as
/// positive integers before they touch the remote command (Rule #3).
#[tauri::command]
pub(crate) async fn insights_hygiene_kill(
    state: State<'_, AppState>,
    workspace_id: String,
    pids: Vec<i32>,
) -> Result<String, String> {
    if pids.is_empty() || pids.len() > 200 || pids.iter().any(|&p| p <= 0) {
        return Err("invalid pid list".into());
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let cmd = format!(
        "curl -s --max-time 8 -X POST -H 'Content-Type: application/json' \
         -H \"Authorization: Bearer $(cat '{home}/.winmux/server/token' 2>/dev/null)\" \
         -d '{{\"pids\":[{list}]}}' \
         'http://127.0.0.1:7879/hygiene/kill'"
    );
    let out = exec(&handle, &cmd, 12).await?;
    crate::dlog_tag("MONITOR", &format!("hygiene kill pids={} → {}", pids.len(), out.trim()));
    Ok(out)
}

#[tauri::command]
pub(crate) async fn addon_logs(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<String, String> {
    let handle = pick_handle(&state, &workspace_id)
        .ok_or("no active SSH session for this workspace")?;
    let home = remote_home(&handle).await;
    let log = match id.as_str() {
        ids::INSIGHTS => format!("{home}/.winmux/server/insights.log"),
        ids::HOOKS => format!("{home}/.winmux/hook-debug.log"),
        ids::NGINX_PROXY => format!("{home}/.winmux/logs/mobile-install.log"),
        _ => return Ok(String::new()),
    };
    exec(&handle, &format!("tail -n 200 \"{log}\" 2>/dev/null || true"), 10).await
}

#[cfg(test)]
mod nginx_proxy_tests {
    use super::valid_domain;

    #[test]
    fn accepts_real_domains() {
        for d in ["winmux.example.com", "a.b.co", "my-server.dev", "x1.y2.example.org"] {
            assert!(valid_domain(d), "should accept {d}");
        }
    }

    #[test]
    fn rejects_bad_domains() {
        for d in [
            "", "nodot", "no_underscores.com", "UPPER.com", ".leading.com",
            "trailing.com.", "-hyphen.com", "spa ce.com", "a..b.com",
            "semi;rm-rf.com", "$(whoami).com",
        ] {
            assert!(!valid_domain(d), "should reject {d:?}");
        }
    }
}
