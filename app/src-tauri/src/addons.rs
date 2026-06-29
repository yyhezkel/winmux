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
fn pick_handle(state: &AppState, workspace_id: &str) -> Option<Arc<SshHandle<SshClient>>> {
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
async fn exec(handle: &SshHandle<SshClient>, cmd: &str, timeout_secs: u64) -> Result<String, String> {
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

async fn remote_home(handle: &SshHandle<SshClient>) -> String {
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
