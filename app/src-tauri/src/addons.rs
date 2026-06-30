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

// Phase 68.C: the cross-compiled insights daemon, embedded so the AddonManager
// can SFTP-upload the arch-matched binary on install (no GitHub release needed
// for testing; the eventual release can switch to fetch).
const INSIGHTS_X64: &[u8] = include_bytes!("../resources/winmux-insights-linux-x64");
const INSIGHTS_ARM64: &[u8] = include_bytes!("../resources/winmux-insights-linux-arm64");

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
            exec(
                handle,
                &format!("\"{home}/.winmux/bin/winmux-insights\" --version 2>/dev/null | head -1"),
                8,
            )
            .await
        }
        routines::INSIGHTS_INSTALL => insights_install(handle, home).await,
        routines::INSIGHTS_UNINSTALL => {
            exec(
                handle,
                &format!(
                    "systemctl --user disable --now winmux-insights 2>/dev/null; \
                     pkill -f 'winmux-insights serve' 2>/dev/null; \
                     rm -f \"{home}/.winmux/bin/winmux-insights\"; echo removed"
                ),
                15,
            )
            .await
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
async fn exec_stdin(
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
set -euo pipefail
DOMAIN="$1"
# Cloudflare token comes on stdin (never argv).
IFS= read -r CF_TOKEN || true
if [ -z "$CF_TOKEN" ]; then echo "winmux: empty Cloudflare token" >&2; exit 2; fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq nginx certbot python3-certbot-dns-cloudflare >/dev/null

install -d -m 700 /etc/winmux
umask 177
printf 'dns_cloudflare_api_token = %s\n' "$CF_TOKEN" > /etc/winmux/cloudflare.ini
chmod 600 /etc/winmux/cloudflare.ini

if [ ! -d "/etc/letsencrypt/live/$DOMAIN" ]; then
  certbot certonly --dns-cloudflare \
    --dns-cloudflare-credentials /etc/winmux/cloudflare.ini \
    --dns-cloudflare-propagation-seconds 30 \
    -d "$DOMAIN" --non-interactive --agree-tos \
    --register-unsafely-without-email >/dev/null 2>&1 || {
      echo "winmux: certbot failed — check the domain + Cloudflare token" >&2; exit 3; }
fi

SITE="/etc/nginx/sites-available/winmux-$DOMAIN"
cat > "$SITE" <<NGINX
server {
  listen 443 ssl http2;
  server_name $DOMAIN;
  ssl_certificate /etc/letsencrypt/live/$DOMAIN/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/$DOMAIN/privkey.pem;
  ssl_protocols TLSv1.2 TLSv1.3;
  add_header Strict-Transport-Security "max-age=31536000" always;
  limit_req_zone \$binary_remote_addr zone=winmux:10m rate=20r/s;
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
nginx -t >/dev/null 2>&1
systemctl reload nginx || systemctl restart nginx

mkdir -p /etc/letsencrypt/renewal-hooks/post
printf '#!/bin/bash\nsystemctl reload nginx\n' > /etc/letsencrypt/renewal-hooks/post/winmux-reload-nginx.sh
chmod +x /etc/letsencrypt/renewal-hooks/post/winmux-reload-nginx.sh
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
    let prefix = resolve_privilege(handle).await?;
    let script_path = format!("{home}/.winmux/run/nginx-install.sh");
    let _ = exec(handle, &format!("mkdir -p \"{home}/.winmux/run\""), 8).await;
    sftp_upload(handle, &script_path, NGINX_INSTALL_SCRIPT.as_bytes()).await?;
    // domain is validated → safe as a quoted arg; token is on stdin only.
    let cmd = format!("{prefix}bash \"{script_path}\" \"{domain}\"");
    let out = exec_stdin(handle, &cmd, cf_token.as_bytes(), 180).await?;
    let _ = exec(handle, &format!("rm -f \"{script_path}\""), 8).await;
    if out.contains("WINMUX_NGINX_OK") {
        Ok(format!("nginx + TLS ready for {domain}"))
    } else {
        Err(format!(
            "nginx install did not confirm success: {}",
            out.trim().lines().last().unwrap_or("(no output)")
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
        INSIGHTS_ARM64
    } else {
        INSIGHTS_X64
    };
    let _ = exec(
        handle,
        &format!("mkdir -p \"{home}/.winmux/bin\" \"{home}/.winmux/insights\""),
        8,
    )
    .await;
    let final_path = format!("{home}/.winmux/bin/winmux-insights");
    let tmp = format!("{final_path}.tmp");
    sftp_upload(handle, &tmp, bytes).await?;
    let _ = exec(
        handle,
        &format!("chmod 0755 \"{tmp}\" && mv -f \"{tmp}\" \"{final_path}\""),
        10,
    )
    .await;
    // Prefer a user systemd unit (auto-restart, survives logout if lingering
    // is on); fall back to nohup where systemd --user isn't usable.
    let start = format!(
        r#"mkdir -p "{home}/.config/systemd/user"
cat > "{home}/.config/systemd/user/winmux-insights.service" <<'UNIT'
[Unit]
Description=winmux insights daemon
After=network.target
[Service]
ExecStart={final_path} serve
Restart=on-failure
RestartSec=5
[Install]
WantedBy=default.target
UNIT
if command -v systemctl >/dev/null 2>&1 && systemctl --user daemon-reload 2>/dev/null; then
  systemctl --user enable winmux-insights 2>&1 >/dev/null
  # restart (not just start) so an update picks up the new binary
  systemctl --user restart winmux-insights 2>&1 && echo "started (systemd --user)"
else
  pkill -f 'winmux-insights serve' 2>/dev/null
  nohup "{final_path}" serve >/dev/null 2>&1 &
  echo "started (nohup)"
fi"#
    );
    let r = exec(handle, &start, 20).await?;
    Ok(format!("installed; {}", r.trim().lines().last().unwrap_or("")))
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
    pick: impl Fn(&AddonManifest) -> AddonAction,
) -> Result<AddonStatus, String> {
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
        let mut s = status_for(&m, &handle, &home).await;
        s.last_error = Some(e);
        return Ok(s);
    }
    Ok(status_for(&m, &handle, &home).await)
}

#[tauri::command]
pub(crate) async fn addon_install(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, |m| m.install.clone()).await
}

#[tauri::command]
pub(crate) async fn addon_uninstall(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, |m| m.uninstall.clone()).await
}

#[tauri::command]
pub(crate) async fn addon_update(
    state: State<'_, AppState>,
    workspace_id: String,
    id: String,
) -> Result<AddonStatus, String> {
    run_lifecycle(&state, &workspace_id, &id, |m| m.update.clone()).await
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
    let cmd = format!(
        "curl -s --max-time 6 \
         -H \"Authorization: Bearer $(cat '{home}/.winmux/insights/token' 2>/dev/null)\" \
         'http://127.0.0.1:7879{path}'"
    );
    exec(&handle, &cmd, 10).await
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
         -H \"Authorization: Bearer $(cat '{home}/.winmux/insights/token' 2>/dev/null)\" \
         -d '{{\"cmd\":\"{action}\"}}' \
         'http://127.0.0.1:7879/docker/{container_id}/action'"
    );
    exec(&handle, &cmd, 12).await
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
        ids::INSIGHTS => format!("{home}/.winmux/insights/insights.log"),
        ids::HOOKS => format!("{home}/.winmux/hook-debug.log"),
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
