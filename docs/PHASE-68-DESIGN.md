# Phase 68 — Server Insights + unified Add-on framework (DESIGN)

> Status: **planning only** (per Yossi). No code until this doc is approved.
> Priority: last in the current queue (after v0.2.9 cut, Phase 66 round 2,
> Phase 67). This doc is the contract we build against.

## 1. Motivation

Two related needs:

1. **Visibility** — a server sometimes goes slow because something
   (often a broken Docker container) eats RAM/CPU, and today winmux gives
   no way to *see* that without dropping to a shell and running `top` /
   `docker stats`. We want a Monitor surface.
2. **Uniform install** — winmux already pushes several "add-ons" to a
   remote (the CLI binary, the tmux.conf, the Claude hooks), but each has
   its own ad-hoc install path in `winmux-bootstrap` / `cli/hooks.rs`.
   Adding Insights as yet another bespoke installer would compound the
   mess. We want ONE framework that installs/updates/removes/detects any
   add-on, for both freshly-provisioned and already-existing servers.

So Phase 68 = an **Add-on framework** (68.A/B) + its first big new add-on,
the **`winmux-insights` daemon** (68.C), + the **desktop UIs** that drive
them (68.D Monitor, 68.E Add-ons settings, 68.F wizard step).

## 2. Architecture overview

```
┌─ winmux desktop (Windows, Tauri) ──────────────────────────────────────┐
│  Settings → Add-ons tab        Monitor window (📊)                       │
│        │                              │                                  │
│        ▼ addon_* Tauri cmds           ▼ insights_* Tauri cmds            │
│  ┌──────────────────────┐      ┌───────────────────────────┐            │
│  │ AddonManager (Rust)  │      │ Insights client (Rust)     │            │
│  │  - registry of mani- │      │  - HTTP over reverse tunnel│            │
│  │    fests             │      └───────────────────────────┘            │
│  │  - install/detect    │                  │                            │
│  └──────────┬───────────┘                  │ GET /current … (localhost  │
│             │ ssh_exec (install scripts)    │  on remote, via tunnel)    │
└─────────────┼──────────────────────────────┼────────────────────────────┘
              │ SSH                            │ reverse tunnel (existing)
┌─────────────▼──────────────────────────────▼────────────────────────────┐
│ remote server                                                            │
│   ~/.winmux/bin/winmux           (addon: winmux-cli)                      │
│   ~/.winmux/tmux.conf            (addon: tmux-conf)                       │
│   ~/.claude/settings.json hooks  (addon: hooks)                          │
│   ~/.winmux/bin/winmux-insights  (addon: insights) ── systemd/user svc   │
│        └── SQLite at ~/.winmux/insights/metrics.db                        │
│        └── HTTP API on 127.0.0.1:PORT                                     │
└──────────────────────────────────────────────────────────────────────────┘
```

Key reuse: the **reverse SSH tunnel already exists** (Phase 6/47) for the
CLI↔desktop RPC. The Insights HTTP API is exposed the same way — bound to
`127.0.0.1:PORT` on the remote and reached through a tunnelled local
forward. No new inbound ports on the server.

## 3. 68.A — Add-on framework

### 3.1 Manifest schema

One manifest per add-on. Built-in add-ons ship their manifest compiled
into the desktop (a `const`); the schema is also serialisable so a future
"community add-ons" directory can drop JSON files.

```rust
// crates/winmux-addons/src/lib.rs  (new pure crate, like winmux-policy)
#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct AddonManifest {
    pub id: String,            // stable key: "hooks" | "tmux-conf" | "winmux-cli" | "insights"
    pub name: String,          // display name: "Claude Code Hooks"
    pub description: String,
    /// Version this desktop build ships / can install.
    pub version: String,
    /// Other add-on ids that must be installed first (e.g. insights → winmux-cli).
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// How each lifecycle step is performed. Either a shell snippet run via
    /// ssh_exec, or a reference to a built-in Rust routine (for the ones
    /// that need SFTP upload / settings.json edits, not just shell).
    pub install: AddonAction,
    pub uninstall: AddonAction,
    #[serde(default)]
    pub update: Option<AddonAction>,
    /// Prints the installed version to stdout, or nothing/non-zero if absent.
    pub detect: AddonAction,
    /// Does this add-on need sudo to install? Drives the UI warning + the
    /// wizard's "needs admin" gating.
    #[serde(default)]
    pub needs_sudo: bool,
}

#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddonAction {
    /// Run this shell snippet on the remote (ssh_exec). `${WINMUX_BIN}` and
    /// `${REMOTE_HOME}` are substituted before exec.
    Shell { script: String },
    /// Call a built-in Rust routine by name — for add-ons that need SFTP
    /// upload or structured settings.json edits (cli, tmux-conf, hooks).
    Builtin { routine: String },
    /// No-op (e.g. an add-on with no update step).
    Noop,
}
```

The **runtime status** of an add-on on a given workspace (separate from
the manifest):

```rust
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct AddonStatus {
    pub id: String,
    pub installed: bool,
    pub installed_version: Option<String>, // from `detect`
    pub available_version: String,         // from the manifest
    pub update_available: bool,            // installed && installed_version < available
    pub busy: bool,                        // an install/update is in flight
    pub last_error: Option<String>,
}
```

### 3.2 Tauri commands

```
addon_list(workspace_id)            -> Vec<AddonStatus>   // runs each detect over SSH
addon_install(workspace_id, id)     -> AddonStatus        // resolves deps first
addon_uninstall(workspace_id, id)   -> AddonStatus
addon_update(workspace_id, id)      -> AddonStatus
addon_logs(workspace_id, id)        -> String             // tail of the add-on's log, if any
```

All return `Result<_, String>` (Rule #6). Install resolves `dependencies`
topologically and refuses if a dep needs sudo and the session lacks it
(surfaced as `last_error`). Concurrency: an in-flight op sets `busy` and
the manager rejects a second op on the same (workspace, id).

### 3.3 AddonManager

Lives desktop-side (`src/addons.rs`). Holds the built-in manifest registry
and an in-memory per-workspace status cache. `detect` runs are cheap shell
commands batched over one SSH exec channel where possible. The manager
reuses `pick_ssh_handle_for_workspace` (same as file_manager / port-watch).

## 4. 68.B — Migrate existing installers to add-ons

Wrap today's bespoke logic as `Builtin` add-ons (no behaviour change, just
re-homed so the framework + UI can drive them):

| id           | today                                                   | detect                                            | needs_sudo |
|--------------|---------------------------------------------------------|---------------------------------------------------|------------|
| `winmux-cli` | `winmux-bootstrap::bootstrap` (SFTP upload + symlink)   | `~/.winmux/bin/winmux --version`                  | no         |
| `tmux-conf`  | `ensure_tmux_conf` (SFTP upload, hash-gated)            | `sha256sum ~/.winmux/tmux.conf` vs manifest       | no         |
| `hooks`      | `winmux setup-hooks` (Phase 66; settings.json edits)    | read `winmux_meta.hooks_version` in settings.json | no         |
| `insights`   | NEW (68.C)                                               | `winmux-insights --version` + service-active check | usually*  |

\* insights needs sudo only to install a *system* systemd unit; without
sudo it falls back to a `systemd --user` unit (or a nohup-launched
process), so it's installable on locked-down accounts too.

Migration keeps the auto-install-on-bootstrap behaviour (Phase 66.B) but
routes it through `addon_install` so there's a single code path.

## 5. 68.C — `winmux-insights` daemon (Go)

A new, separate Go binary (not Rust) — Go's static cross-compile + tiny
runtime fits a <50 MB-RAM agent, and `gopsutil` + the Docker SDK give us
metrics + container control with little code.

### 5.1 Responsibilities
- Sample every N seconds (default 5s): CPU %, per-core, load avg; RAM
  (used/free/cached/swap); disk (per-mount usage + IO); network (per-iface
  rx/tx rate); top processes (pid, name, cpu%, rss).
- Docker (if the socket is reachable): `docker stats --no-stream`
  equivalent via the SDK + `docker ps -a`.
- Persist to SQLite (rolling 7-day retention).
- Serve a localhost HTTP API.
- Run as a service (systemd system or `--user`), restart-on-failure.

### 5.2 HTTP API (bound 127.0.0.1:PORT, default 7879)

Auth: a bearer token generated at install, stored remote-side at
`~/.winmux/insights/token` (mode 600) and desktop-side in the workspace
record. Same model as the tunnel HMAC (Rule #8 — never logged).

```
GET  /healthz                       -> { ok, version, uptime_s }
GET  /current                       -> Snapshot (see below)
GET  /history?metric=cpu&since=ISO&step=60  -> { points: [{t, v}] }
GET  /docker                        -> { containers: [DockerContainer] }
POST /docker/{id}/action            -> { ok }   body: { cmd: "start|stop|restart|kill" }
GET  /processes?sort=cpu&limit=20   -> { processes: [Proc] }
```

```jsonc
// Snapshot (GET /current)
{
  "ts": "2026-06-28T09:00:00Z",
  "cpu": { "pct": 37.2, "per_core": [..], "load": [1.2, 0.9, 0.7] },
  "mem": { "total": 8e9, "used": 5.1e9, "cached": 1.2e9, "swap_used": 0 },
  "disks": [{ "mount": "/", "total": ..., "used": ..., "pct": 61.0 }],
  "net":  [{ "iface": "eth0", "rx_bps": 12000, "tx_bps": 4000 }],
  "docker_running": 3, "docker_total": 5,
  "top": [{ "pid": 991, "name": "node", "cpu": 22.1, "rss": 5.0e8 }]
}
// DockerContainer
{ "id":"ab12", "name":"web", "image":"nginx", "state":"running",
  "cpu_pct": 12.4, "mem_used": 2.1e8, "mem_pct": 2.6, "restarts": 0,
  "status":"Up 3 days" }
```

### 5.3 SQLite schema

```sql
-- one row per sample tick; wide table keeps writes cheap
CREATE TABLE samples (
  ts          INTEGER NOT NULL,        -- unix seconds
  cpu_pct     REAL, load1 REAL,
  mem_used    INTEGER, mem_total INTEGER, swap_used INTEGER,
  net_rx_bps  INTEGER, net_tx_bps INTEGER
);
CREATE INDEX idx_samples_ts ON samples(ts);

-- per-disk (a sample tick has several rows)
CREATE TABLE disk_samples (
  ts INTEGER, mount TEXT, used INTEGER, total INTEGER
);
CREATE INDEX idx_disk_ts ON disk_samples(ts);

-- docker per-container per-tick (only while containers exist)
CREATE TABLE docker_samples (
  ts INTEGER, cid TEXT, name TEXT, cpu_pct REAL, mem_used INTEGER, state TEXT
);
CREATE INDEX idx_docker_ts ON docker_samples(ts);
```

Retention: a sweep on startup + hourly `DELETE FROM … WHERE ts < now-7d;`
then `PRAGMA wal_checkpoint(TRUNCATE)`. WAL mode, `synchronous=NORMAL`
(durability across power-loss isn't critical for metrics). Downsampling
(1-min rollups beyond 24h) is a v2 nicety — note it, don't build it round 1.

### 5.4 Resource budget & guardrails
- Target **<50 MB RSS, <1% CPU avg**. Sampling at 5s with gopsutil is
  cheap; the SQLite writes are the main cost — batch each tick in one tx.
- Self-limit: if the DB exceeds e.g. 200 MB, drop the oldest day early.
- Log to `~/.winmux/insights/insights.log`, size-rotated (e.g. 5×1 MB).
- `GET /history` caps `step`/range so a huge query can't OOM the daemon.

## 6. 68.D — Monitor UI (desktop)

A floating window (reuse the `.fm-window` chrome, like the GG MD viewer)
titled "📊 Server Insights — <workspace>". **Pull-based**: refresh on open
and on an explicit Refresh button + an optional 5s auto-refresh toggle
(off by default, so an idle window costs nothing).

```
┌ 📊 Server Insights — prod ────────────────────────────── ⟳  □  × ┐
│ CPU  ▁▂▅▇▆▅▃  37%      RAM  ████████░░ 64% (5.1/8 GB)            │
│ Disk /  ██████░ 61%    Net  ↓12 KB/s ↑4 KB/s                     │
├─ Docker (3/5 running) ──────────────────────────────────────────┤
│  ● web    nginx     12% cpu   2% mem   Up 3d   [stop][restart][⋯]│
│  ● api    node      ⚠️88% cpu 41% mem  Up 1d   [stop][restart][⋯]│  ← alert row
│  ○ db     postgres  —          —       Exited  [start]      [⋯]  │
├─ Top processes ─────────────────────────────────────────────────┤
│  node  pid 991   22% cpu  500 MB  │  postgres pid 77  8% …       │
└──────────────────────────────────────────────────────────────────┘
```

- 4 metric mini-charts (CPU/RAM/Disk/Net). Chart lib: **uPlot** (tiny,
  fast) preferred over recharts/chart.js for the <KB sparkline use; revisit
  if we want richer interaction.
- Docker table: sortable; row turns ⚠️ when cpu>80% **or** mem>80% of host.
  Per-row Start/Stop/Restart/Kill (confirm on Kill) + a Logs button
  (streams `docker logs --tail` via a new endpoint or the existing pane).
- Alerts are display-only round 1 (no push). Push-to-mobile ties into
  Phase 67 later.

## 7. 68.E — Settings → Add-ons UI

New tab in Settings (scoped per active workspace — add-ons are
per-server). Table driven by `addon_list`:

```
Add-on              Installed   Available   Status        Actions
──────────────────────────────────────────────────────────────────
winmux CLI          0.2.9       0.2.9       ✓ up to date  [Reinstall]
tmux config         274a97f6    274a97f6    ✓ up to date  [Update][Remove]
Claude Code Hooks   1.1.0       1.2.0       ⬆ update       [Update][Remove][Logs]
Server Insights     —           1.0.0       not installed [Install]
```

Install/Update/Remove call the matching `addon_*` command; rows show a
spinner while `busy`, and surface `last_error` inline. "Logs" opens the
add-on's log tail.

## 8. 68.F — Add-on selection in wizards

- **Provision new server** (`ProvisioningWizard` "new" mode): after the
  configure step, an "Add-ons to install" checklist (all available,
  sensible defaults ticked: cli+tmux-conf+hooks on; insights opt-in).
  Runs them through `addon_install` as part of execute.
- **Connect to existing server** (`ConnectExistingFlow`): after the user
  picker + key install, run `addon_list` and show "detected vs missing",
  offering to install the missing ones. This finally gives the
  connect-existing path the same tooling story as fresh provisioning.

## 9. Security considerations
- Insights API is **localhost-only** on the remote + bearer-token auth +
  reached solely through the existing authenticated reverse tunnel. No new
  inbound exposure.
- `POST /docker/{id}/action` is the only state-changing endpoint — it can
  start/stop/kill containers. Token-gated; the desktop confirms
  destructive actions (Kill). Never expose to non-loopback.
- Token stored mode-600 remote, DPAPI-or-memory desktop (Rule #2/#8),
  never logged.
- Add-on install scripts run over SSH as the workspace user; `needs_sudo`
  add-ons prompt/inform. Build scripts use arg-arrays / `shell_quote`
  (Rule #3) — never naive string concat.
- The Go daemon parses only its own SQLite + the Docker socket; it does
  not exec arbitrary input.

## 10. Open questions (need Yossi's call before build)
1. **Insights distribution** — bundle `winmux-insights` (Go) into the
   desktop resources like `winmux-linux-x64` (adds ~8-12 MB to the
   installer per arch), or fetch from GitHub releases on first install?
   *(Recommend: bundle x64, fetch arm64 — mirrors the CLI story.)*
2. **Go dependency** — OK to introduce a second language/toolchain (Go) to
   the build, or should Insights be Rust to keep one toolchain? *(Go is
   faster to write here, but Rust reuses our cross-build infra. Lean
   Rust if you'd rather not add Go to CI.)*
3. **Default port 7879** for the Insights API — OK? (CLI RPC tunnel is
   separate; this is a second forwarded port.)
4. **Auto-refresh default** in the Monitor — off (pull-only) as drafted,
   or a gentle 5s while the window is focused?
5. **Scope of round 1** — do we ship 68.A+B+C+D (framework + daemon +
   monitor) first and defer 68.E/F (settings + wizard integration) to a
   round 2, or all at once?
6. **Chart lib** — uPlot (tiny) vs recharts (familiar, heavier). 

---
*When approved, suggested build order: 68.A (framework + winmux-addons
crate) → 68.B (migrate existing, no behaviour change, verify) → 68.C
(insights daemon + API + SQLite) → 68.D (monitor UI) → 68.E (settings) →
68.F (wizard).* Each is its own branch off the v0.2.x line; nothing here
blocks v0.2.9.
