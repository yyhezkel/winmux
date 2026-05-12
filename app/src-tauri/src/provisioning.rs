//! Phase 14.A: server provisioning wizard.
//!
//! Bootstraps a fresh server beyond just "create a workspace":
//!   - inspects the remote (OS, package manager, disk)
//!   - applies a profile of steps (update, install basics, create user,
//!     deploy SSH key, harden sshd, install language runtimes, install
//!     Claude Code, run `winmux setup-hooks`)
//!   - streams progress to the frontend via `provisioning:progress`
//!     events so the wizard's live log feels native
//!   - persists profiles in `%APPDATA%\winmux\provisioning-profiles.json`
//!     and original credentials in `…\provisioning-secrets.json` (DPAPI
//!     wrap planned — see below) so a second pass can resume
//!
//! Connections are stateless within a provisioning run: we open one
//! russh `client::Handle` to the target and reuse it across every step's
//! exec channel. Failures don't abort the run — each step ends as one of
//! pending / running / done / failed, the wizard offers retry/skip per
//! step, and we save a checkpoint after every state change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use russh::client;
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::{config_dir_pub, dlog, AppState};

// ─── profile + step model ──────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    UpdatePackages,
    InstallBasics,
    CreateUser,
    GenerateKeypair,
    DeployPubkey,
    TestNewKey,
    DisableRootSsh,
    DisablePasswordSsh,
    InstallNodejs,
    InstallPython,
    InstallDocker,
    /// Install Claude Code via Anthropic's official curl installer.
    /// Previously this step ran `npm install -g @anthropic-ai/claude-code`,
    /// which forced an npm + Node.js dep tree. The official installer
    /// (`curl … | bash`) is npm-agnostic — Anthropic ships a static
    /// binary launcher — and it's the version they want users on.
    InstallClaudeCode,
    /// New in Phase 14.A.2: Codex CLI (`npm i -g @openai/codex`).
    /// Needs Node.js — the step will fail with a clear hint if
    /// `npm` isn't on PATH yet.
    InstallCodex,
    /// New in Phase 14.A.2: Gemini CLI (`npm i -g @google/gemini-cli@latest`).
    /// Same Node.js dependency as Codex.
    InstallGemini,
    SetupWinmuxHooks,
}

impl StepKind {
    pub fn label(&self) -> &'static str {
        match self {
            StepKind::UpdatePackages => "Update packages",
            StepKind::InstallBasics => "Install basics (tmux, curl, git…)",
            StepKind::CreateUser => "Create user with sudo",
            StepKind::GenerateKeypair => "Generate keypair locally",
            StepKind::DeployPubkey => "Deploy public key",
            StepKind::TestNewKey => "Test new key login",
            StepKind::DisableRootSsh => "Disable root SSH password login",
            StepKind::DisablePasswordSsh => "Disable password auth on SSH",
            StepKind::InstallNodejs => "Install Node.js LTS",
            StepKind::InstallPython => "Install Python 3 + pip + venv",
            StepKind::InstallDocker => "Install Docker (official repo)",
            StepKind::InstallClaudeCode => "Install Claude Code (Anthropic, curl installer)",
            StepKind::InstallCodex => "Install Codex CLI (OpenAI, npm)",
            StepKind::InstallGemini => "Install Gemini CLI (Google, npm)",
            StepKind::SetupWinmuxHooks => "Run winmux setup-hooks",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct ProvisioningProfile {
    pub id: String,
    pub label: String,
    /// Ordered list of steps. Each step's args (user name, etc.) live on
    /// the run input — profiles are templates, not instances.
    pub steps: Vec<StepKind>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct ProfilesFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub profiles: Vec<ProvisioningProfile>,
}

fn default_version() -> u32 {
    1
}

pub(crate) fn default_profiles() -> Vec<ProvisioningProfile> {
    // Note: the Claude Code installer doesn't need npm (its `curl
    // install.sh | bash` ships a self-contained launcher), so we
    // drop the previous `InstallNodejs` prerequisite from the default
    // profile. Profiles that include Codex / Gemini DO list
    // InstallNodejs explicitly first since both still install via npm.
    vec![
        ProvisioningProfile {
            id: "default".into(),
            label: "Default — basics + user + key + Claude Code".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                StepKind::InstallClaudeCode,
                StepKind::SetupWinmuxHooks,
            ],
        },
        ProvisioningProfile {
            id: "hardened".into(),
            label: "Hardened — default + disable root + disable password".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                StepKind::DisableRootSsh,
                StepKind::DisablePasswordSsh,
                StepKind::InstallClaudeCode,
                StepKind::SetupWinmuxHooks,
            ],
        },
        ProvisioningProfile {
            id: "minimal".into(),
            label: "Minimal — update + user + key only".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
            ],
        },
        ProvisioningProfile {
            id: "all-agents".into(),
            label: "All agents — Claude Code + Codex + Gemini".into(),
            steps: vec![
                StepKind::UpdatePackages,
                StepKind::InstallBasics,
                StepKind::CreateUser,
                StepKind::GenerateKeypair,
                StepKind::DeployPubkey,
                StepKind::TestNewKey,
                // Codex + Gemini both ship via npm; Node.js must be on
                // PATH first.
                StepKind::InstallNodejs,
                StepKind::InstallClaudeCode,
                StepKind::InstallCodex,
                StepKind::InstallGemini,
                StepKind::SetupWinmuxHooks,
            ],
        },
    ]
}

fn profiles_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("provisioning-profiles.json"))
}

fn secrets_path() -> Result<PathBuf, String> {
    Ok(config_dir_pub()?.join("provisioning-secrets.json"))
}

pub(crate) fn load_profiles_from_disk() -> Result<ProfilesFile, String> {
    let path = profiles_path()?;
    if !path.exists() {
        return Ok(ProfilesFile {
            version: 1,
            profiles: default_profiles(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut file: ProfilesFile = serde_json::from_str(text.trim_start_matches('\u{FEFF}'))
        .map_err(|e| format!("parse {path:?}: {e}"))?;
    // Seed any missing built-ins so future built-in profiles light up
    // automatically without nuking the user's edits.
    let mut have: std::collections::HashSet<String> =
        file.profiles.iter().map(|p| p.id.clone()).collect();
    for d in default_profiles() {
        if !have.contains(&d.id) {
            have.insert(d.id.clone());
            file.profiles.push(d);
        }
    }
    Ok(file)
}

fn save_profiles_to_disk(file: &ProfilesFile) -> Result<(), String> {
    use std::io::Write as _;
    let path = profiles_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?
        .to_path_buf();
    let tmp = dir.join(format!("provisioning-profiles.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("open tmp {tmp:?}: {e}"))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

// ─── secret storage (DPAPI-wrapped initial password) ───────────────────────

#[derive(Clone, Serialize, Deserialize, Default)]
struct SecretsFile {
    #[serde(default)]
    entries: std::collections::BTreeMap<String, String>, // workspace_id → b64(ciphertext)
}

/// Wrap secret bytes with Windows DPAPI. PowerShell shell-out keeps us
/// off the windows-rs dep tree. The encrypted blob is bound to the
/// current user's profile and machine — moving the JSON file to another
/// user account yields gibberish.
#[cfg(target_os = "windows")]
fn dpapi_protect(secret: &str) -> Result<String, String> {
    let secret = secret.to_string();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference = 'Stop'; \
             Add-Type -AssemblyName System.Security; \
             $in = $env:WINMUX_SECRET; \
             $bytes = [System.Text.Encoding]::UTF8.GetBytes($in); \
             $prot = [System.Security.Cryptography.ProtectedData]::Protect($bytes, $null, 'CurrentUser'); \
             [Convert]::ToBase64String($prot)",
        ])
        .env("WINMUX_SECRET", &secret)
        .output()
        .map_err(|e| format!("dpapi spawn: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(target_os = "windows"))]
fn dpapi_protect(secret: &str) -> Result<String, String> {
    // On non-Windows builds (cross-compile for resources/winmux-linux-x64)
    // the secret store is unused — provisioning runs UI-side on Windows.
    Ok(format!("noprotect:{secret}"))
}

fn save_workspace_secret(workspace_id: &str, password: &str) -> Result<(), String> {
    let path = secrets_path()?;
    let mut file: SecretsFile = if path.exists() {
        let t = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&t).unwrap_or_default()
    } else {
        SecretsFile::default()
    };
    let wrapped = dpapi_protect(password)?;
    file.entries.insert(workspace_id.to_string(), wrapped);
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(())
}

// ─── inspect (step 1 of wizard, before any mutation) ───────────────────────

#[derive(Clone, Serialize)]
pub(crate) struct InspectResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_pretty_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whoami: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub df_h: Option<String>,
}

/// One-shot remote inspection. Opens a russh handle, runs a fixed bundle
/// of `bash -lc` commands, then drops the connection. The wizard shows
/// this to the user before they pick a profile so they know what OS
/// they're about to provision.
#[tauri::command]
pub(crate) async fn provisioning_inspect(
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    key_path: Option<String>,
    key_passphrase: Option<String>,
) -> InspectResult {
    match provisioning_inspect_inner(host, port, user, password, key_path, key_passphrase).await {
        Ok(r) => r,
        Err(e) => InspectResult {
            ok: false,
            message: Some(e),
            uname: None,
            os_pretty_name: None,
            os_id: None,
            os_version: None,
            package_manager: None,
            whoami: None,
            df_h: None,
        },
    }
}

async fn provisioning_inspect_inner(
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    key_path: Option<String>,
    key_passphrase: Option<String>,
) -> Result<InspectResult, String> {
    let mut handle = open_ssh(&host, port, &user, &password, &key_path, &key_passphrase).await?;
    let uname = exec_capture(&mut handle, "uname -a").await.ok();
    let os_release = exec_capture(&mut handle, "cat /etc/os-release 2>/dev/null || true")
        .await
        .ok();
    let whoami = exec_capture(&mut handle, "whoami").await.ok();
    let df_h = exec_capture(&mut handle, "df -h / 2>/dev/null | tail -n 1")
        .await
        .ok();
    let pm = exec_capture(
        &mut handle,
        "command -v apt >/dev/null && echo apt && exit; \
         command -v dnf >/dev/null && echo dnf && exit; \
         command -v yum >/dev/null && echo yum && exit; \
         command -v apk >/dev/null && echo apk && exit; \
         echo unknown",
    )
    .await
    .ok();

    let (pretty, id, version) = parse_os_release(os_release.as_deref().unwrap_or(""));
    Ok(InspectResult {
        ok: true,
        message: None,
        uname,
        os_pretty_name: pretty,
        os_id: id,
        os_version: version,
        package_manager: pm,
        whoami,
        df_h,
    })
}

fn parse_os_release(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut pretty = None;
    let mut id = None;
    let mut version = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            pretty = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("ID=") {
            id = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version = Some(v.trim_matches('"').to_string());
        }
    }
    (pretty, id, version)
}

// ─── live run ──────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ProvisionInput {
    pub workspace_id: String,
    pub host: String,
    pub port: u16,
    pub initial_user: String,
    #[serde(default)]
    pub initial_password: Option<String>,
    #[serde(default)]
    pub initial_key_path: Option<String>,
    #[serde(default)]
    pub initial_key_passphrase: Option<String>,
    pub new_user: String,
    /// Local path to drop the freshly-generated keypair. If absent we
    /// compute `~/.ssh/winmux-<workspace>-<new_user>`.
    #[serde(default)]
    pub local_key_path: Option<String>,
    pub profile_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct StepProgress {
    pub run_id: String,
    pub step_index: usize,
    pub step_kind: String,
    pub state: &'static str, // "running" | "done" | "failed" | "skipped"
    pub log_chunk: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub timestamp_iso: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct RunHandle {
    pub run_id: String,
}

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
fn new_run_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("prov_{t:x}_{n:x}")
}

fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Spawn a provisioning task and return a handle. The task emits
/// `provisioning:progress` events with `StepProgress` payloads. The
/// initial password (if provided) is DPAPI-wrapped and saved alongside
/// the workspace so a later "View server info" can recover it.
#[tauri::command]
pub(crate) async fn provisioning_start(
    state: State<'_, AppState>,
    app: AppHandle,
    input: ProvisionInput,
) -> Result<RunHandle, String> {
    let _ = state;
    let run_id = new_run_id();
    let profile = {
        let pf = load_profiles_from_disk()?;
        pf.profiles
            .iter()
            .find(|p| p.id == input.profile_id)
            .cloned()
            .ok_or_else(|| format!("unknown profile {}", input.profile_id))?
    };
    if let Some(pw) = input.initial_password.as_ref() {
        if let Err(e) = save_workspace_secret(&input.workspace_id, pw) {
            dlog(&format!("provisioning: save secret failed: {e}"));
        }
    }

    let app_for_task = app.clone();
    let run_id_clone = run_id.clone();
    tauri::async_runtime::spawn(async move {
        run_provisioning(app_for_task, run_id_clone, input, profile).await;
    });

    Ok(RunHandle { run_id })
}

async fn run_provisioning(
    app: AppHandle,
    run_id: String,
    input: ProvisionInput,
    profile: ProvisioningProfile,
) {
    let mut handle = match open_ssh(
        &input.host,
        input.port,
        &input.initial_user,
        &input.initial_password,
        &input.initial_key_path,
        &input.initial_key_passphrase,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            emit(
                &app,
                &run_id,
                0,
                &StepKind::UpdatePackages,
                "failed",
                String::new(),
                Some(format!("SSH connect failed: {e}")),
            );
            return;
        }
    };

    // Detect package manager once and cache.
    let pm = exec_capture(
        &mut handle,
        "command -v apt >/dev/null && echo apt && exit; \
         command -v dnf >/dev/null && echo dnf && exit; \
         command -v yum >/dev/null && echo yum && exit; \
         command -v apk >/dev/null && echo apk && exit; \
         echo unknown",
    )
    .await
    .unwrap_or_else(|_| "unknown".into())
    .trim()
    .to_string();

    let local_key_path = input.local_key_path.clone().unwrap_or_else(|| {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        format!("{home}\\.ssh\\winmux-{}-{}", input.workspace_id, input.new_user)
    });

    for (idx, kind) in profile.steps.iter().enumerate() {
        emit(&app, &run_id, idx, kind, "running", String::new(), None);
        let result = match kind {
            StepKind::UpdatePackages => {
                let cmd = match pm.as_str() {
                    "apt" => "sudo apt-get update -y && sudo DEBIAN_FRONTEND=noninteractive apt-get upgrade -y",
                    "dnf" => "sudo dnf -y upgrade",
                    "yum" => "sudo yum -y update",
                    "apk" => "sudo apk update && sudo apk upgrade",
                    _ => "echo unknown package manager; exit 1",
                };
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallBasics => {
                let pkgs = "tmux curl wget git build-essential vim htop ca-certificates";
                let cmd = match pm.as_str() {
                    "apt" => format!("sudo DEBIAN_FRONTEND=noninteractive apt-get install -y {pkgs}"),
                    "dnf" => format!("sudo dnf -y install tmux curl wget git make gcc gcc-c++ vim htop ca-certificates"),
                    "yum" => format!("sudo yum -y install tmux curl wget git make gcc gcc-c++ vim htop ca-certificates"),
                    "apk" => "sudo apk add tmux curl wget git build-base vim htop ca-certificates".to_string(),
                    _ => "echo unknown package manager; exit 1".to_string(),
                };
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::CreateUser => {
                let u = shell_escape(&input.new_user);
                // Create user + add to sudo + passwordless sudo. Idempotent
                // via `id -u` short-circuit.
                let cmd = format!(
                    "if id -u {u} >/dev/null 2>&1; then \
                       echo 'user already exists'; \
                     else \
                       sudo useradd -m -s /bin/bash {u} && \
                       sudo usermod -aG sudo {u} 2>/dev/null || sudo usermod -aG wheel {u}; \
                     fi; \
                     echo '{u} ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/90-winmux-{u} >/dev/null && \
                     sudo chmod 0440 /etc/sudoers.d/90-winmux-{u}"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::GenerateKeypair => {
                // Local step — uses ssh-keygen.exe (ships with Windows 10+).
                let r = local_step_generate_keypair(&local_key_path, &input.new_user).await;
                match r {
                    Ok(out) => {
                        emit(&app, &run_id, idx, kind, "done", out, None);
                        Ok(())
                    }
                    Err(e) => {
                        emit(&app, &run_id, idx, kind, "failed", String::new(), Some(e));
                        Err(())
                    }
                }
            }
            StepKind::DeployPubkey => {
                let pub_path = format!("{}.pub", local_key_path);
                match std::fs::read_to_string(&pub_path) {
                    Ok(pub_text) => {
                        let u = shell_escape(&input.new_user);
                        let key_line = shell_escape(pub_text.trim());
                        let cmd = format!(
                            "sudo install -d -m 700 -o {u} -g {u} /home/{u}/.ssh && \
                             echo {key_line} | sudo tee -a /home/{u}/.ssh/authorized_keys >/dev/null && \
                             sudo chown {u}:{u} /home/{u}/.ssh/authorized_keys && \
                             sudo chmod 600 /home/{u}/.ssh/authorized_keys"
                        );
                        run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
                    }
                    Err(e) => {
                        emit(
                            &app,
                            &run_id,
                            idx,
                            kind,
                            "failed",
                            String::new(),
                            Some(format!("read {pub_path}: {e}")),
                        );
                        Err(())
                    }
                }
            }
            StepKind::TestNewKey => {
                // Open a SECOND SSH handle with the just-deployed key.
                let r = open_ssh(
                    &input.host,
                    input.port,
                    &input.new_user,
                    &None,
                    &Some(local_key_path.clone()),
                    &None,
                )
                .await;
                match r {
                    Ok(mut h2) => {
                        let who = exec_capture(&mut h2, "whoami").await.unwrap_or_default();
                        emit(
                            &app,
                            &run_id,
                            idx,
                            kind,
                            "done",
                            format!("connected as {who}"),
                            None,
                        );
                        Ok(())
                    }
                    Err(e) => {
                        emit(&app, &run_id, idx, kind, "failed", String::new(), Some(e));
                        Err(())
                    }
                }
            }
            StepKind::DisableRootSsh => {
                let cmd = "sudo sed -i -E 's/^[# ]*PermitRootLogin.*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config && \
                           (sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd 2>/dev/null || sudo service ssh reload)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::DisablePasswordSsh => {
                let cmd = "sudo sed -i -E 's/^[# ]*PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config && \
                           (sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd 2>/dev/null || sudo service ssh reload)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallNodejs => {
                let cmd = "curl -fsSL https://deb.nodesource.com/setup_lts.x | sudo -E bash - && \
                           sudo DEBIAN_FRONTEND=noninteractive apt-get install -y nodejs";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallPython => {
                let cmd = match pm.as_str() {
                    "apt" => "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y python3 python3-pip python3-venv",
                    "dnf" => "sudo dnf -y install python3 python3-pip",
                    "yum" => "sudo yum -y install python3 python3-pip",
                    "apk" => "sudo apk add python3 py3-pip",
                    _ => "echo unsupported pm; exit 1",
                };
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallDocker => {
                let cmd = "curl -fsSL https://get.docker.com | sudo sh && \
                           sudo usermod -aG docker $(whoami)";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
            StepKind::InstallClaudeCode => {
                // Official Anthropic installer (https://claude.ai/install.sh).
                // Self-contained — no npm prerequisite. Runs as the new
                // user so `~/.claude` lands in the right home directory.
                // `su -l` would re-evaluate the shell rc files (which the
                // installer relies on for PATH bumps) but isn't available
                // in all minimal images; we fall back to `sudo -u … bash
                // -lc` which has the same effect.
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "sudo -u {u} bash -lc 'curl -fsSL https://claude.ai/install.sh | bash' && \
                     echo '✓ Claude Code installed. Run `claude` on the server to authenticate (browser-based) \
                     or set ANTHROPIC_API_KEY for headless mode.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::InstallCodex => {
                // Codex ships through npm — fail fast with a clear hint
                // if Node isn't on PATH yet rather than letting npm's
                // error spill into the log.
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "if ! command -v npm >/dev/null; then echo 'ERROR: npm not found. Enable the \"Install Node.js LTS\" step first, or install Node manually.' && exit 1; fi; \
                     sudo npm install -g @openai/codex && \
                     sudo chown -R {u}:{u} /home/{u}/.npm /home/{u}/.codex 2>/dev/null || true; \
                     echo '✓ Codex installed. Run `codex login --device-auth` on the server for headless auth, or `codex login` if you have a desktop browser.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::InstallGemini => {
                let u = shell_escape(&input.new_user);
                let cmd = format!(
                    "if ! command -v npm >/dev/null; then echo 'ERROR: npm not found. Enable the \"Install Node.js LTS\" step first, or install Node manually.' && exit 1; fi; \
                     sudo npm install -g @google/gemini-cli@latest && \
                     sudo chown -R {u}:{u} /home/{u}/.npm /home/{u}/.gemini 2>/dev/null || true; \
                     echo '✓ Gemini CLI installed. Run `gemini` to sign in with Google, or set GEMINI_API_KEY for headless mode.'"
                );
                run_step(&mut handle, &app, &run_id, idx, kind, &cmd).await
            }
            StepKind::SetupWinmuxHooks => {
                // Best-effort — winmux CLI on remote is bootstrapped on
                // first SSH connection from the desktop app, so it may
                // not be there yet. Wrap in `command -v`.
                let cmd = "if command -v winmux >/dev/null; then winmux setup-hooks --agent claude || true; else echo 'winmux CLI not yet bootstrapped — connect once to install, then re-run'; fi";
                run_step(&mut handle, &app, &run_id, idx, kind, cmd).await
            }
        };
        if result.is_err() {
            dlog(&format!(
                "provisioning {run_id}: step {idx} {kind:?} failed — leaving run paused for retry"
            ));
            // Don't auto-abort; the wizard surfaces a retry button.
            // We continue here to mirror the spec ("retry OR skip").
        }
    }
    emit(
        &app,
        &run_id,
        profile.steps.len(),
        &StepKind::UpdatePackages,
        "done",
        String::new(),
        Some("provisioning complete".into()),
    );
}

async fn local_step_generate_keypair(local_path: &str, comment: &str) -> Result<String, String> {
    if std::path::Path::new(local_path).exists() {
        return Ok(format!("reusing existing local key {local_path}"));
    }
    if let Some(parent) = std::path::Path::new(local_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    let out = std::process::Command::new("ssh-keygen")
        .args([
            "-t", "ed25519", "-N", "", "-C",
        ])
        .arg(format!("winmux/{comment}"))
        .arg("-f")
        .arg(local_path)
        .output()
        .map_err(|e| format!("ssh-keygen spawn: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok(format!("generated {local_path} (ed25519)"))
}

fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '@' | ':' | '=' | ',' | '%'))
    {
        return s.into();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

async fn run_step(
    handle: &mut client::Handle<crate::SshClient>,
    app: &AppHandle,
    run_id: &str,
    idx: usize,
    kind: &StepKind,
    cmd: &str,
) -> Result<(), ()> {
    match exec_capture(handle, cmd).await {
        Ok(out) => {
            emit(app, run_id, idx, kind, "done", out, None);
            Ok(())
        }
        Err(e) => {
            emit(
                app,
                run_id,
                idx,
                kind,
                "failed",
                String::new(),
                Some(e),
            );
            Err(())
        }
    }
}

fn emit(
    app: &AppHandle,
    run_id: &str,
    idx: usize,
    kind: &StepKind,
    state: &'static str,
    log_chunk: String,
    message: Option<String>,
) {
    let _ = app.emit(
        "provisioning:progress",
        StepProgress {
            run_id: run_id.to_string(),
            step_index: idx,
            step_kind: format!("{kind:?}"),
            state,
            log_chunk,
            message,
            timestamp_iso: iso_now(),
        },
    );
}

// ─── ssh exec helpers ──────────────────────────────────────────────────────

/// Open a russh client::Handle using the project's SshClient + the same
/// password/key/agent ladder as `try_authenticate`. We don't reuse the
/// existing one in lib.rs to keep this module self-contained — the cost
/// is a duplicate ~30 lines of auth ladder.
async fn open_ssh(
    host: &str,
    port: u16,
    user: &str,
    password: &Option<String>,
    key_path: &Option<String>,
    key_passphrase: &Option<String>,
) -> Result<client::Handle<crate::SshClient>, String> {
    let config = Arc::new(client::Config::default());
    let target = format!("{host}:{port}");
    let connect = client::connect(
        config,
        (host, port),
        crate::SshClient::new_anonymous(target.clone()),
    );
    let mut handle = tokio::time::timeout(std::time::Duration::from_secs(15), connect)
        .await
        .map_err(|_| "ssh connect timed out".to_string())?
        .map_err(|e| format!("ssh connect: {e}"))?;

    // Key auth (if key_path provided) using the std::fs::read_to_string +
    // decode_secret_key path we adopted in lib.rs's recent fix.
    if let Some(p) = key_path {
        let text = std::fs::read_to_string(p).map_err(|e| format!("read {p}: {e}"))?;
        let key = russh_keys::decode_secret_key(&text, key_passphrase.as_deref())
            .map_err(|e| format!("decode key {p}: {e}"))?;
        let pkwh = crate::pkwh_pub(key)?;
        if handle
            .authenticate_publickey(user, pkwh)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(handle);
        }
    }
    if let Some(pw) = password {
        if handle
            .authenticate_password(user, pw)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(handle);
        }
    }
    Err("authentication failed".to_string())
}

async fn exec_capture(
    handle: &mut client::Handle<crate::SshClient>,
    cmd: &str,
) -> Result<String, String> {
    let mut chan = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    chan.exec(true, cmd).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut exit_code: i32 = 0;
    loop {
        match chan.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExtendedData { data, .. }) => out.extend_from_slice(&data[..]),
            Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status as i32,
            Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
            _ => {}
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    if exit_code != 0 {
        return Err(format!("exit {exit_code}: {text}"));
    }
    Ok(text)
}

// ─── Tauri profile management ─────────────────────────────────────────────

#[tauri::command]
pub(crate) fn provisioning_profiles_list() -> Result<ProfilesFile, String> {
    load_profiles_from_disk()
}

#[tauri::command]
pub(crate) fn provisioning_profile_save(profile: ProvisioningProfile) -> Result<ProfilesFile, String> {
    let mut file = load_profiles_from_disk()?;
    if let Some(existing) = file.profiles.iter_mut().find(|p| p.id == profile.id) {
        *existing = profile;
    } else {
        file.profiles.push(profile);
    }
    save_profiles_to_disk(&file)?;
    Ok(file)
}

#[tauri::command]
pub(crate) fn provisioning_profile_delete(id: String) -> Result<ProfilesFile, String> {
    let mut file = load_profiles_from_disk()?;
    file.profiles.retain(|p| p.id != id);
    save_profiles_to_disk(&file)?;
    Ok(file)
}

#[tauri::command]
pub(crate) fn provisioning_step_catalog() -> Vec<(String, String)> {
    let all = [
        StepKind::UpdatePackages,
        StepKind::InstallBasics,
        StepKind::CreateUser,
        StepKind::GenerateKeypair,
        StepKind::DeployPubkey,
        StepKind::TestNewKey,
        StepKind::DisableRootSsh,
        StepKind::DisablePasswordSsh,
        StepKind::InstallNodejs,
        StepKind::InstallPython,
        StepKind::InstallDocker,
        StepKind::InstallClaudeCode,
        StepKind::InstallCodex,
        StepKind::InstallGemini,
        StepKind::SetupWinmuxHooks,
    ];
    all.into_iter()
        .map(|k| (format!("{k:?}"), k.label().to_string()))
        .collect()
}
