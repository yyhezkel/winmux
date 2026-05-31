# Decisions & Open Threads

Append-only log of ideas raised, decisions made, and threads in flight. Move entries between sections; don't delete history.

## How to use

Three sections:
- **Open** — questions or ideas awaiting a decision or action
- **Decided** — resolved entries with outcome
- **Archived** — things that became irrelevant or superseded

Entry format:

```
### YYYY-MM-DD — Short title
- **Context:** what came up
- **Options:** A / B / C (brief tradeoffs)
- **Status / Decision:** where things stand
- **Outcome / Commit:** hash + phase if implemented
```

When starting a session, scan **Open** first. Surface anything that's been pending too long.

---

## Open

### 2026-05-27 — Competitive-scan ideas inventory (triage in progress)
- **Context:** Survey of 8 winmux GitHub projects produced an inventory of ~25 ideas to potentially adopt. Highest-impact triple: HTTP Automation API for LLM control (#2.1), Auto port forwarding (#2.2), Secrets Vault (#3.2).
- **Source docs:** `docs/COMPETITIVE-SCAN.md` (full report + Secrets Vault design), `docs/IDEAS-RANKING.md` (decision table).
- **Status:** 5 MUST closed in Phase 35 (`bddc0b0`), auto port forwarding closed in Phase 36 / 36.A (`95de6f1`). 3 MUST remain (Pipe hardening deferred, Secrets Vault deferred → external MCP integration, Full LLM control on the roadmap), plus 10 SHOULD + 6 COULD. As individual items are decided, move them to their own Decided / Open entries with phase + commit references. Master inventory stays here until fully triaged.
- **Also flagged:** the `winmux` name is taken by 8 projects on GitHub — rebrand caveat (see scan doc's "Naming Caveat" section).

### 2026-05-27 — Bidi mixed-content rendering
- **Context:** Hebrew + Latin tokens (DEV, MAIN) in Claude Code CLI output mislead readers visually in xterm.js. User confirmed approach 1+2.
- **Plan:**
  - 33A — `<code>` styling + `<bdi>` wrapping for technical tokens on HTML surfaces (chat, editor, modals, file manager)
  - 33B — opt-in PTY bidi filter for Claude Code panes (FSI/PDI around Latin runs in plain text lines; leave ANSI escapes and box-drawing untouched)
- **Status:** Paused while Help discovery (Phase 34) shipped first. Resume next.

### 2026-05-27 — PATH auto-registration in the WiX/NSIS installer
- **Context:** v0.1.0 README "Coming next" item. Splits off from the now-closed long-standing roadmap block because it's small and in scope (everything else in that block went to out-of-scope).
- **Effort:** ~1 hour. Modify the NSIS installer script to add winmux's install dir to PATH on install, remove on uninstall.
- **Status:** Open, low priority but cheap to ship.

### 2026-05-27 — Full LLM control of the app (= scan #2.1, HTTP Automation API)
- **Context:** Today Claude runs *inside* winmux panes; goal is to also let Claude *drive* winmux from the outside — "open me a pane on server X and run cargo build", "read me the scrollback of pane #3", "screenshot the workspace", etc.
- **Foundation already in place:** `rpc_server.rs` (JSON-RPC v2 over Named Pipe), 15 browser-automation tools in `winmux-mcp.exe`, methods for list-workspaces / tree / send / send-key / feed.push.
- **Gap to close:**
  - New RPC methods: `pane.scrollback`, `pane.screenshot`, `ui.tree`, `action.split`, `action.connect`
  - Expose via `winmux-mcp.exe` as new tools: `read_pane`, `take_screenshot`, `list_panes`, `split_pane`, `connect_workspace`
  - Optional: HTTP endpoint on 127.0.0.1 with auth token for scripters who don't want MCP
- **Effort:** ~5-7 days (per scan estimate).
- **Sources:** `docs/COMPETITIVE-SCAN.md` §2.1 (full design borrowed from editnori/WinMux's `NativeAutomationServer.cs`), `docs/IDEAS-RANKING.md` row 2.1 (✅ MUST).
- **Status:** Moved to bottom of Open 2026-05-28 — Yossi: "not ready yet, keep on roadmap." The big-ticket next focus once the Sprint 1 quick wins settle.

---

## Decided

### 2026-05-31 — Ports redesign: detect-only, click-to-forward, per-port stop, sanity-check (Phase 46, `1aef47f`)
- **Context:** Yossi tested v0.2.5 ports end-to-end. Five problems: notifications popping for every auto-forward; the toggle being too eager (forwarded everything detected); no way to stop a tunnel; clicking a forwarded port opened a browser tab that couldn't reach `localhost:<port>`; parallel-workspace behavior unverified.
- **Decision:**
  - **Detect ≠ forward.** `AppState.detected_ports` is a new runtime-only registry separate from `forwards`. `port.opened` inserts into detected + emits `port-detected` (no auto-forward, no FeedItem). `port.closed` removes from detected + emits `port-undetected`, and tears down the forward if one was open.
  - **Click to forward.** New `forward_port_start` command looks up addr from detected_ports and calls `open_auto_forward`. PortsWindow rows now have two states: Detected → `[Forward]` button (click row/button → forward + open browser); Forwarded → `[Open]` + `[Stop]`.
  - **Sanity-probe before "live".** `open_auto_forward` now runs a 200 ms TCP probe to `127.0.0.1:<local_port>` between binding and emitting `port-forwarded`. On failure it tears down and returns an error — the FE never opens a dead browser tab.
  - **Use `127.0.0.1` not `localhost`** for browser URLs. Root cause of issue 4 was dual-stack `localhost` → `::1` while the russh forward is IPv4-only.
  - **FeedItem removed** — the PortsWindow is the only surface; no toasts, no feed cards.
  - Event names renamed for clarity: `port-forward-opened` → `port-forwarded`; `port-forward-closed` → `port-forward-stopped`.
- **Multi-workspace:** all state already keyed by `(workspace_id, remote_port)`; per-workspace listener tasks are independent. Confirmed by code review; Phase 44's per-handler dlog telemetry will surface any future contention.
- **Tests:** new `tcp_probe_tests` module (probe succeeds for listening port, fails for vacant port). 41 Rust tests total.
- **Outcome / Commit:** `1aef47f` (debug build green, BUILD_EXIT 0, app.exe ~31.0 MB). 8 files changed, 397+/84-. 454 i18n keys × 4 locales (parity OK; `ports.notification.opened` retired). No version bump — batches into v0.2.6.

### 2026-05-31 — Pipe listener pool (8 slots) + handler lifetime telemetry (Phase 44, `c65b0c5`)
- **Context:** Yossi reported sporadic `tunnel: bridge error … os error 231` on v0.2.5 — much less frequent than the pre-39.A storm, but still present. Diagnosis traced it to the rpc_server's accept loop: Phase 39.A raised `max_instances` to 254 and pre-created the "next" listener, but at any single moment there was still only ONE listener in accept state. Two clients racing for it within microseconds raced for that single slot; the loser got `ERROR_PIPE_NOT_AVAILABLE` (231). `max_instances(254)` is the OS upper bound on instances, NOT concurrent accept-ready listeners.
- **Decision:**
  - `LISTENER_POOL_SIZE = 8` simultaneous listeners, each in its own tokio task that loops `make_listener → connect() → tokio::spawn(handle_client_with_telemetry(...)) → recreate`. The handler runs on a separate task so the slot recreates its listener immediately — never blocked on handler duration. A burst of up to 8 concurrent clients serves instantly; the 9th+ still falls into tunnel.rs's bounded backoff (rare). max_instances(254) ceiling and the Phase 39.C `catch_unwind` are unchanged.
  - Handler-lifetime telemetry: each connection now dlogs `handler {id} START` and `handler {id} END {ms} ms` with a 5-hex sequential connection id (atomic counter, no new deps). Future support tickets surface slow handlers immediately.
- **Test:** `pool_serves_concurrent_clients_without_busy` — spins up an 8-slot mini-pool on a unique pipe name and fires 4 simultaneous `ClientOptions::new().open()` calls; asserts all succeed (no 231). 39 Rust tests pass total.
- **Spec deviation:** `handle_client` returns `()` and exits on either EOF or read error with no top-level distinction, so the END dlog logs duration only (no result-kind classification). Reframing as `Result<_, String>` would touch every break path without changing observability.
- **Outcome / Commit:** `c65b0c5` (debug build green, BUILD_EXIT 0, app.exe ~30.9 MB). Single backend file changed; no version bump — batches into v0.2.6.

### 2026-05-31 — i18n mop-up: 7 untranslated leftovers + 27 hardcoded strings (Phase 43, `2204226`)
- **Context:** v0.2.5 audit found 100% key parity across en/he/ar/ru (431 keys each), but 7 he/ar/ru values still matched English (untranslated leftovers — Buffer, Toasts, Shell, Host, three tmux_picker keys) and 27 user-visible strings in SettingsModal/CreateWorkspaceModal/App/Sidebar were hardcoded in JSX, bypassing `t()`.
- **Decision:**
  - **Part A** — translated the 9 identical-value entries in he/ar/ru (`settings.terminal.buffer`, `settings.notifications.toasts`, `ws.create.field.shell` + `ws.create.shell.label`, `ws.create.field.host` + `provisioning.field.host`, `tmux_picker.{pane_id_target, window, label_secondary}`). For RTL locales the arrow in `pane_id_target` flips (← vs →). en stays canonical.
  - **Part B** — converted all 27 hardcoded strings to `t()`: SettingsModal ×19 (section titles + section spans + 2 placeholders), CreateWorkspaceModal ×4 (user/KEY/value/remove attributes), App ×2 (error-boundary fallbacks), Sidebar ×1 (workspace-connected title).
- **Deviation from spec key names:** 14 of the SettingsModal hardcoded strings already had matching snake_case keys with full translations (`settings.theme.preset`, `settings.font.ui`, `settings.hooks.enabled`, `settings.updates.check_on_startup`, etc.) — the JSX simply wasn't calling them. Reused those instead of adding camelCase duplicates per the spec table. Kept only `settings.terminal.buffer.title` and `settings.notifications.toasts.title` as new keys (spec explicitly differentiated them from existing siblings). Net: 11 new keys (placeholders, env, errors, sidebar title, the two `.title` variants) instead of the 25 the spec table implied.
- **Outcome / Commit:** `2204226` (debug build green, BUILD_EXIT 0, app.exe ~30.9 MB). Parity verified post-write: **448 keys × 4 locales**. Pure frontend; no version bump — batches into v0.2.6.

### 2026-05-31 — CreateWorkspaceModal sizing: wider, height-capped, internal scroll (Phase 42, `7610312`)
- **Context:** The New-/Edit-workspace modal grew taller than typical viewports — the title bled off the top and the Save/Cancel row off the bottom. Old sizing was `min-width: 420px; max-width: 520px;` with no height cap.
- **Decision:** Add a scoped `.ws-create-modal` class and turn the modal into a vertical flex container: `max-width: min(820px, 92vw)`, `max-height: min(92vh, 720px)`, `padding: 0`. The `<h3>` pins as header with its own padding + bottom border, a new `.ws-create-modal-body` wraps the middle as the `overflow-y: auto` scroller, and the Save/Cancel row pins as `.ws-create-modal-footer` (border-top + padding, overriding the default `.modal-buttons` `margin-top`). A `.ws-form-grid` 2-col grid wraps the SSH user/host/port labels (`.full-width` opt-out for long fields); collapses to 1 col under 640px. Scoped strictly to `.ws-create-modal` so the seven other `.modal` users (Settings, Notes, Ports, FileEditor, ProvisioningWizard, SshKeyOffer, PaneView) are untouched.
- **Outcome / Commit:** `7610312` (debug build green, BUILD_EXIT 0, app.exe ~30.9 MB). CSS-only + minimal JSX wrappers; no version bump — batches into v0.2.6.

### 2026-05-29 — Auto-connect SSH on workspace select (Phase 41, `dc4b1d7`)
- **Context:** Activating an SSH workspace did nothing until the user opened a terminal pane — so the tmux session picker showed empty (`Ok([])`) and the file manager errored (`connect a terminal pane first`). Yossi wanted the background connection to fire on select so those populate immediately.
- **Decision (Approach A, confirmed with Yossi):** Factored `spawn_ssh`'s connect → host-key → authenticate sequence into a shared `connect_and_authenticate` helper (carries the Phase 38 keepalive). New `SshSession.tx: Option` represents a **headless** session — handle + workspace_id, no PTY — which the tmux picker and file manager already key off (they only read `handle`). New idempotent `workspace_ensure_connected` command: no-op if a session exists; connects **agent/key only** (`password: None`, `accept_unknown_host: false`); re-checks under the lock and drops the spare if a pane connected mid-auth; skips silently with a dlog otherwise (password-mode workspaces connect when a pane opens, as before). `Settings.auto_connect_on_workspace_select` (serde-default **true**, backwards-compatible). App.tsx `createEffect(activeWs)` fires the command once per workspace switch when the setting is on and the workspace is SSH (guarded so the initial workspace still fires after settings loads async). SettingsModal toggle + hint (Terminal tab); i18n ×4.
- **Trade-off accepted:** one extra background SSH connection per active SSH workspace (keepalive keeps it healthy; bounded — idempotent, at most one headless per workspace). Reconnect-on-dead-handle is out of scope (separate roadmap item).
- **Outcome / Commit:** `dc4b1d7` (debug build green, BUILD_EXIT 0, app.exe ~30.9 MB; 38 Rust tests incl. a default/backwards-compat unit test). Frontend+backend; no version bump — batches into v0.2.6.

### 2026-05-29 — Ports feature redesign: sidebar button + current-workspace window (Phase 40, `aea8438`)
- **Context:** Phase 39.E hid the sidebar Ports entry after live testing showed the global PortsWindow was usually empty. Step 1 of a multi-step Ports rebuild: bring the entry back, but tighter and scoped.
- **Decision:** Re-added the 🌐 Ports sidebar button next to [📝 Notes][⚙ Settings] with an explanatory `title` tooltip ("Lets your local browser reach what's running on the remote server"). PortsWindow rewritten to show ONLY the active workspace — dropped the "All workspaces" tab — and auto-tracks workspace switches via `activeWs()`. Prominent Active/Inactive auto-forward toggle row at the top (colored by the workspace's color, green fallback; disabled for Local workspaces since they have no SSH), then the forwards list or one of three contextual empty states (toggle-on-no-forwards / toggle-off / no-workspace). Badge click now activates the workspace, then opens the window. Added a short `sidebar.ports.label` key for the button text so the long sentence lives only in the `title` (the old key doubled as the label). i18n in all 4 locales; tab keys removed.
- **Outcome / Commit:** `aea8438` (debug build green, BUILD_EXIT 0, app.exe ~30.9 MB). Frontend-only; no version bump — batches into v0.2.6. Further Ports-rebuild steps to follow.

### 2026-05-28 — Ports default-off + internal-port filter + floating Ports/Notes + Logs tab (Phase 39, `3a5c50b`)
- **Context:** Live testing of v0.2.4 surfaced six related issues, the headline being a foot-gun: auto port forwarding was on by default and the Phase 36 watcher forwarded EVERY remote LISTEN port — including winmux's own reverse-tunnel HMAC endpoint — so the browser hit `WINMUX-CHALLENGE / WINMUX-DENIED bad-format`.
- **Decision:**
  - `auto_port_forward` default flipped true→false (opt-in per workspace).
  - Backend tracks winmux's reverse-tunnel remote ports (`internal_reverse_tunnel_remote_ports`, inserted on `tcpip_forward`, removed on session end); `port.opened` skips them. CLI watcher also self-excludes the `WINMUX_SOCKET_ADDR` port.
  - Ports moved from a sidebar panel to a floating PortsWindow (scoped + All-workspaces tabs), opened from the per-workspace 🌐 badge. `PortsPanel.tsx` deleted.
  - Sidebar bottom reshaped: [Notes][Settings] paired row, then New workspace, then Provision.
  - Settings gains a Logs tab: live 200-line `debug.log` tail (`read_log_tail` seeks 256 KB from EOF), 5s auto-refresh, path + Open folder + Copy path. The bare Logs row in Updates is removed.
  - Notes scoped per-workspace. **Implementation deviation:** kept the existing per-note `workspace_id` field (window filters to active ws; legacy null-workspace notes stay visible everywhere; workspace-delete drops the ws's notes and warns first) rather than physically relocating storage into `workspaces.json` as the spec suggested — same behavior, far less migration risk. Flagged for review.
- **Outcome:** Phase 39 shipped `3a5c50b` (build green, app.exe ~30.8 MB). Ships in v0.2.5 — cut TBD.
- **Phase 39.A (`4f466c0`):** follow-up after Yossi hit an `ERROR_PIPE_NOT_AVAILABLE (231)` storm (~3/sec) — the v0.2.4 remote CLI has no `WINMUX_SOCKET_ADDR` self-exclude, so it hammered the RPC pipe. Three server-side robustness fixes: (1) `rpc_server` pipe `max_instances` 8 → 255; (2) accept loop pre-creates the next listener before spawning the handler (no 1-listener race window); (3) `tunnel.rs` bridge retries 231 with bounded backoff (25→800ms, `tracing::debug` per attempt, dlog only on give-up). Also added a global `[🌐 Ports]` sidebar button (opens PortsWindow on the All-workspaces tab; per-workspace badge unchanged). The remote-CLI root cause clears when v0.2.5 rebakes the bundled Linux binary with the Phase 39 self-exclude.
- **Phase 39.B (`67021a0`):** the missing piece for *existing* installs. Phase 39 only changed the default for NEW workspaces; workspaces.json entries created before the flip still had `auto_port_forward: true` and kept auto-forwarding (the actual storm source on Yossi's machine). Added a one-time startup migration — `Settings.migrations.phase_39_auto_port_forward_default_flipped` guards a single pass that flips every existing `true` workspace to `false` (only when `load_state == Loaded`; logs the count). A later per-workspace opt-in survives because the flag stops the migration re-running. Silent (no toast). With 39 + 39.A + 39.B + the v0.2.5 CLI rebake, the storm is closed at the source on existing installs.
- **Phase 39.C (`06c0104`):** regression fix — 39.A set pipe `max_instances(255)`, but tokio's `ServerOptions::max_instances()` *panics* at construction ("cannot specify more than 254 instances"), so the pipe server never started and every tunnel bridge hit `ERROR_FILE_NOT_FOUND (os error 2)`. Dropped to 254 (the wrapper's hard max) and extracted `make_listener()` wrapping the builder in `catch_unwind` so a future tokio limit change degrades to a logged `max_instances(100)` fallback instead of crashing the server task. Test (`#[tokio::test]`) constructs the listener twice and asserts both build. Last unreleased piece before v0.2.5 can ship safely.
- **Phase 39.E (`88253b1`):** sidebar Ports button removed — feature accessible via the per-workspace badge until full re-evaluation. The standalone 39.A globe button mostly opened an empty PortsWindow in live testing; hidden until the feature is handled end-to-end. PortsWindow, the per-workspace `🌐 N` badge, `auto_port_forward`, the 39.B migration, and the 39.A/C/D backend robustness all remain.
- **Phase 39.D (`e42cb83`):** atomic temp-file upload + pkill stale watcher to dodge ETXTBSY on CLI re-upload. After the Phase 39 CLI rebuild changed the bundled binary's hash, every reconnect re-triggered the SFTP upload and hit `sftp create …winmux-linux-x64: Failure: Failure` — Linux returns ETXTBSY when truncating a still-executing binary (a leftover port-watch from the pre-39.C pipe crash kept the old CLI running), which OpenSSH maps to the generic `SSH_FX_FAILURE`. `upload_via_sftp` now `pkill -f winmux-linux-x64` first (reaps orphan watchers), uploads to `…x64.tmp`, then `mv -f` atomic-renames onto the final name (swaps the dir-entry to a fresh inode — succeeds even if the old proc is still alive). Unblocks the port-watch CLI actually deploying to remotes.

### 2026-05-28 — README roadmap refresh shipped (v0.2.4 flow, `990b612`)
- **Context:** The README "Shipped" section was frozen at v0.1.0 (Phases 16–34 unrostered). Tracked as an Open thread since 2026-05-27.
- **Decision/Outcome:** Rewritten during the v0.2.4 release flow as a v0.1.0 → v0.2.4 cumulative summary (3 prose paragraphs) with a current "Coming next" list; out-of-scope items removed. Commit `990b612`. Closes the Open thread.

### 2026-05-28 — SSH keepalive + disconnect logging + Settings/Logs UX (Phase 38, `d4ba544`)
- **Context:** Users reported SSH dropping after a few minutes idle. Diagnosis: all russh `client::Config` sites used the default `keepalive_interval: None` → no keepalive packets → NAT/firewall idle timeouts (5-15 min) silently killed the TCP link. The read loop's `None`/Close/Eof arms also `break` with no log, so debug.log showed nothing. Plus logs were unreachable for non-technical users (no UI exposed the path).
- **Decision:**
  - Enable keepalive on all four russh sites (`keepalive_interval: Some(30s)`, default `keepalive_max=3` → ~90s dead-peer detection).
  - SSH read-loop disconnect paths now `dlog` the reason + channel/pane/workspace ids + `last_activity_ms` (idle age) — distinguishes a keepalive/NAT drop from an active-session close.
  - Sidebar gains a ⚙ Settings button above "New workspace"; the existing settings FAB stays.
  - Settings → Updates tab gains a Logs section (new `log_dir_path` command) with "Open folder" (revealItemInDir) + "Copy path". Closes the former "Logs accessibility for end users" Open thread.
- **Outcome:** Phase 38 shipped `d4ba544` (build green, app.exe ~30.8 MB). Released in v0.2.5 — cut TBD.

### 2026-05-28 — Workspace creation: password-only mode genuinely allowed (Phase 37, `f8a8ebe`)
- **Context:** Form UX implied an SSH key was mandatory; the backend accepted `None` but users couldn't tell. The edit flow also locked connection fields. Bug found post-v0.2.4.
- **Decision:** UI radio between "SSH Key" / "Password (prompted on connect)". Password mode saves no credential (`key_path = null`) — prompted interactively at every connect; the password is never persisted (CLAUDE.md Rule 2; the Connection struct is unchanged, no password field, no DPAPI for SSH passwords). Edit flow now opens host/user/port/key/auth-mode for change: `workspace_update` gained an optional `connection` param that replaces `ws.connection` and rewrites every Terminal pane's connection. The keyless→interactive-password connect path already worked and was left as-is.
- **Outcome:** Phase 37 shipped `f8a8ebe` (build green, app.exe ~30.8 MB). Released in v0.2.5 — cut TBD when Yossi greenlights.

### 2026-05-28 — Auto port forwarding shipped (#2.2, Phase 36, `95de6f1`)
- **Context:** Selected after Sprint 1 as a parallel track to the Secrets Vault research session (separate worktree).
- **Decision:** Linux CLI watcher scans `/proc/net/tcp(6)` every 500ms, diffs LISTEN ports, and reports `port.opened` / `port.closed` over the existing tunnel RPC. The Windows backend opens a russh local-forward bound to the SAME local port as the remote (fallback +1..+9) so `localhost:3000` just works. Per-workspace `auto_port_forward` toggle (default on). Ports panel in the sidebar; one passive FeedItem per opened forward.
- **Watcher launch:** fire-and-forget `winmux port-watch` exec channel per workspace (deduped via `state.port_watchers`), tied to the SSH session lifetime. Idempotent `open_forward_matched` absorbs duplicate opens from multiple panes.
- **Known v1 edge:** if the pane whose SSH session hosts the watcher disconnects while another pane stays connected, auto-forward pauses until reconnect. Acceptable for v1; revisit if it bites.
- **Outcome:** Phase 36 shipped (build green, app.exe ~30.8 MB). Tests: `/proc/net/tcp` parser (5) + forwards-map (2).
- **Phase 36.A:** switched to kernel-allocated ephemeral local ports (`bind 127.0.0.1:0` + `local_addr().port()`); removed remote-port matching and the +1..+9 fallback (no more cross-workspace collision on shared remote ports, no race). UI shows "active on localhost:<port>"; added an inline `🌐 <count>` badge on the workspace sidebar tab (click opens the browser for 1 forward, surfaces the Ports panel for >1). `open_forward_matched` renamed `open_auto_forward`.

### 2026-05-28 — Secrets Vault deferred — waiting on external MCP integration
- **Context:** Yossi is building a separate MCP server that will hold secrets; the Vault content moves there. winmux's role becomes "expose egress capabilities" (SSH env inject, child-process spawn with env) which the external MCP composes.
- **Decision:** Pause all Vault implementation in winmux until the external MCP is ready (Yossi: "hopefully this week"). When ready, plan integration as a separate phase — winmux gets the egress hooks + RPC surface, external MCP holds secrets + does capability protocol.
- **Research preserved:** `docs/SECRETS-VAULT-RESEARCH.md` lives on branch `research/secrets-vault` (commit `c0ace9a`). Don't merge to main yet — reference material until the integration design settles.
- **Status:** Resume after external-MCP communication contract is defined.

### 2026-05-28 — Sprint 1 quick wins shipped (Phase 35, `bddc0b0`)
The first 5 MUST items from the competitive-scan triage, in one phase + build (app.exe 30.6 MB, exit 0):

- **#3.5 CLAUDE.md absolute rules.** Built on top of the existing CLAUDE.md from commit `5774c1a` — added a 12-rule "Do Not Violate" absolute-rules section (no PTY content in logs, no plaintext secrets at rest, no shell-command string concat, no unwrap in prod Rust, no `any` in TS, atomic persistence, …).
- **#1.5 ts-rs shared types.** 21 Rust structs/enums (Workspace, LayoutNode, Connection, PaneKind, FeedItem, Settings + transitive closure) generate TS into `app/src/bindings/`; `types.ts` re-exports them. `cargo test` regenerates. `settings.ts` kept its richer hand-tuned mirror (literal unions). Note: ts-rs renders `Option<T>` as `T | null`, so a few construction sites moved from `undefined` to `null`.
- **#1.1 rAF-coalesced xterm writer.** PTY chunks merged into one `requestAnimationFrame` write; flush+cancel on dispose. Fixes "(Not Responding)" during fast streaming.
- **#1.3 Command Palette.** Ctrl+Shift+P, substring filter over 20 commands, i18n in 4 locales. Reuses existing handlers; `pane.rename` via a window CustomEvent into PaneView.
- **#1.2 OSC 9/99/777 notification detection.** New `osc_notify.rs` stateful parser (BEL + ST terminators, 4 KB cap, 7 unit tests) wired into `emit_data` on both PTY read loops, observe-only; emits `osc-notification` → frontend passive FeedItem. Universal complement to the Claude-specific hooks.

### 2026-05-27 — Rebrand, winget, Scoop, ARM64 Windows, aarch64-linux, code-signing, delta-downloads — all out of scope
- **Context:** Distribution-scale items inherited from the v0.1.0 README "Coming next" list and discovered via the competitive scan (naming caveat).
- **Decision:** All out of scope for now. winmux is an open-source project Yossi builds for his own working convenience — not investing in broad distribution mechanics.
  - Rebrand: 8 winmux projects on GitHub, but namespace collision isn't a problem at the current scale.
  - winget submission: was useful for fast updates, but the v0.2.3 native-HTTP updater already covers Yossi's actual update path.
  - Scoop: same as winget.
  - ARM64 Windows: no ARM users in scope.
  - aarch64-linux CLI: no ARM Linux servers in scope.
  - Code-signing: SmartScreen warning is a distribution problem, not a personal-use problem.
  - Delta downloads: optimization for high-volume releases, not relevant at current scale.
- **Outcome:** All seven items closed. Revisit individually if the project's purpose shifts toward wider distribution.

### 2026-05-27 — MCP server in browser (deferred — separate parallel project)
- **Context:** Original ask was to make winmux's MCP server reachable from the browser. Two options were on the table (auto-register winmux-mcp in claude config, OR remote bridge over SSH tunnel).
- **Decision:** Deferred. Yossi is rebuilding MCP from scratch in a separate project; integration with winmux may happen later, but the existing local browser MCP work isn't being continued in winmux right now.
- **Outcome:** No further work in this repo. Revisit if/when the external MCP project is ready to merge.

### 2026-05-27 — Updater HTTP via native client (v0.2.3, `4e38ad4`)
- **Context:** v0.2.1/v0.2.2 updater shelled out to PowerShell for manifest fetch. AV / Constrained Language Mode on a corporate machine caused parser errors swallowing the request.
- **Decision:** Replace PowerShell shell-outs with `ureq` + `rustls` (pure-Rust TLS — bypasses SChannel too). Moved `sha256_file` to the `sha2` crate.
- **Outcome:** Shipped in v0.2.3. v0.2.1/v0.2.2 installs must download v0.2.3 manually once; auto-update works thereafter.

### 2026-05-27 — Help discovery is focused, not scattered (Phase 34, `5140508`)
- **Context:** Phase 33 had a `?` button on every pane header + an "Open help" split-menu entry. User: "don't scatter `?` icons; help is very focused."
- **Decision:** Remove `?` from pane header. One contextual `?` icon next to the SSH key field in workspace settings; tooltip "Need help? Open guide". SshKeyOfferModal's "Manual setup guide" button stays primary entry.
- **Outcome:** Phase 34 shipped.

### 2026-05-27 — Per-pane identity inheriting from workspace (Phase 31, `07d8d6d`)
- **Context:** Phase 30 shipped per-workspace color/emoji + dynamic window title. User: "this should be per pane."
- **Decision:** Identity at pane level overrides workspace; reset clears pane override (falls back to workspace). Window title follows focused pane's effective identity.
- **Outcome:** Phase 31 shipped.

### 2026-05-19 — Revert ClaudeChat + ClaudeLog (Phase 24.D, `c2106ef`)
- **Context:** Phases 22–24.B built an in-app chat + log render of Claude conversations alongside the terminal pane.
- **Decision:** Reverted. Three competing "talk to claude" UIs felt fragmented. Terminal + tmux remain the canonical interface.
- **Outcome:** Phase 24.D removed both panes; PaneKind aliases fall through to Terminal for backward compat.

---

## Archived

_(empty)_
