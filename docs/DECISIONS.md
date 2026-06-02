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
- **Status:** 5 MUST closed in Phase 35 (`bddc0b0`), auto port forwarding closed in Phase 36 / 36.A (`95de6f1`). Pipe hardening landed in Phase 44 (`c65b0c5`). Phase 48 closed several SHOULD/COULD items: #1.4 Ctrl+Alt+Arrow split-or-move, #3.3 ADR + threat-model docs, #4.9 /doctor diagnostic endpoint, #4.10 frontend stall instrumentation, plus BiDi 33A (HTML-surface tokens). Phase 49 closed: #2.3 drag-drop into terminal, #4.3 auto-destroy empty workspaces, #4.5 git worktree per workspace, #4.6 quadrant split keymap, #4.7 CHANGELOG postmortem habit (lives in CONTRIBUTING.md instead). Phase 50 closed: #2.4 Diff pane. 2 MUST remain (Secrets Vault deferred → external MCP integration, Full LLM control on the roadmap). Still on the inventory: 3.1 lib.rs split, BiDi 33B (PTY-stream FSI/PDI). As individual items are decided, move them to their own Decided / Open entries with phase + commit references. Master inventory stays here until fully triaged.
- **Also flagged:** the `winmux` name is taken by 8 projects on GitHub — rebrand caveat (see scan doc's "Naming Caveat" section).

### 2026-05-27 — BiDi 33B: opt-in PTY filter for Claude Code panes
- **Context:** Phase 48-A shipped 33A (HTML-surface `<TechText>` + `<bdi>` wrapping). 33B remains: an opt-in filter on the PTY byte stream itself that wraps Latin runs in FSI/PDI inside plain text lines, leaving ANSI escapes and box-drawing untouched.
- **Why deferred:** PTY filtering risks breaking display in subtle ways (cursor positioning, line wrapping, color sequences). Wait until 33A has proven out in production before touching xterm.js.
- **Status:** Open, paused. Reassess when there's appetite + a Claude Code workflow that 33A doesn't already cover.

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

### 2026-06-02 — Speech-to-text with local-model option
- **Context:** Yossi wants voice input → text in winmux. Specifically with the option to point at a model running LOCALLY (no cloud dependency).
- **Likely architecture:** native Web Speech API as the default fallback (Chrome/Edge engines provide it for free), plus a configurable endpoint for a local server (Whisper.cpp HTTP, Vosk, etc.). User toggles between them in Settings.
- **Surface ideas:** push-to-talk shortcut in any focused text field (chat, file editor, terminal write-buffer pre-send), or a "Voice input" button.
- **Status:** Open, not yet scoped further. Triage when ready.

---

## Decided

### 2026-06-02 — lib.rs split — POC: pure data types crate (Phase 51.A, `e0d6d71`)
- **Context:** Scan #3.1 (last SHOULD item). lib.rs grew to ~5,300 lines through 50 phases; the scan recommends an 8-crate workspace split. After a scope-check conversation Yossi agreed to a POC-first approach: do the smallest, lowest-coupling extraction (pure data types) as Phase 51.A before committing to the remaining 7 sub-phases.
- **Decision:** New crate `app/src-tauri/crates/winmux-types/` holds 8 wire/persistence types (`Connection`, `SplitDirection`, `PaneKind`, `DiffSource`, `BrowserState`, `LayoutNode`, `EnvVar`, `Workspace`) + the serde-helper functions (`is_terminal_kind`, `default_true`, `is_true`) + `BROWSER_HISTORY_MAX` const + the two `Default` impls. Field visibility raised from `pub(crate)` → `pub` (now cross-crate). Re-exported from `app/src-tauri/src/lib.rs` as `pub(crate) use winmux_types::{...}` so every existing `crate::Connection` / `crate::Workspace` callsite resolves unchanged. Cargo workspace gained `crates/winmux-types` member; `app` declares `winmux-types = { path = "crates/winmux-types" }`.
- **Friction encountered + fix:** ts-rs `export_to` resolves relative to the **source file's directory**, not the manifest. The naive `"../../../src/bindings/"` (matching the manifest depth) wrote bindings to `app/src-tauri/src/bindings/` — wrong location. Corrected to `"../../../../src/bindings/"` (4 ups instead of 3) and removed the misplaced directory. Generated binding contents are byte-identical to pre-split — verified by inspecting `Workspace.ts` and `PaneKind.ts`.
- **Intentionally left in lib.rs** for later sub-phases: `WorkspacesFile`, `CreateInput` (internal-shaped, paired with persist+split helpers), `FeedItem`/`FeedItemState` (going to `winmux-feed`), `Settings`/`MigrationFlags` (already in `settings.rs` as separate module), and all runtime types (`Session`, `AppState`, `ForwardEntry`, etc.) which are coupled to tokio/russh.
- **Validation:** `cargo check` clean. `cargo test --workspace` = **49 passing** (36 app + 5 cli + 8 winmux-types). The 8 in winmux-types are the ts-rs auto-generated `export_bindings_*` synthetic tests that previously ran under `app` (44 → 36 in app, but +8 in the new crate ≡ no test lost). `tsc --noEmit` clean. Debug build BUILD_EXIT 0. No version bump — batches into v0.2.7.
- **POC verdict:** approach scales cleanly. Workspace mechanics fine, re-export pattern preserves all callsites, ts-rs binding regen works once the path-depth gotcha is understood. No circular-dep issues at this layer (data types have no business-logic deps). Ready to proceed with 51.B (the next-smallest extraction, likely `winmux-tunnel` or `winmux-bootstrap`) on Yossi's signal.

### 2026-06-01 — SettingsModal width + General tab (Phase 49.A, `6c4f5e8` + `ffffadb`)
- **Context:** Yossi tested the v0.2.7-pre debug binary and flagged two things: the SettingsModal was too narrow (Hebrew/Arabic hint text wrapped awkwardly), and the Phase 49-C auto-destroy control had landed in the Terminal tab for lack of a better home — same applied to the older auto-connect-on-workspace-select toggle. Both are workspace-lifecycle / app-behavior settings, not terminal-specific.
- **Decision:** Modal widened from 720→900px (`max-width: 92vw`, `max-height: min(92vh, 760px)`); the existing flex-column + `.settings-pane { overflow-y: auto }` already handle the body scroll, so only the outer dimensions changed. A new "general" Tab variant was added at the **first** position in the tab list (leftmost LTR / rightmost RTL), with a single new i18n key `settings.tab.general` × 4 locales (he: כללי, ar: عام, ru: общие). `auto_destroy_empty_workspaces_days` (Phase 49-C) and `auto_connect_on_workspace_select` (Phase 41) both moved to General; their existing i18n keys `settings.autoDestroy.*` / `settings.autoConnect.*` were reused verbatim. Terminal tab no longer carries either control.
- **Two-commit landing:** `6c4f5e8` shipped the General tab + auto-destroy move; `ffffadb` followed with the CSS widening + auto-connect move once Yossi clarified the revised spec. Single-phase number (`49.A`) covers both since they're the same intent.
- **Validation:** tsc clean across both commits, no Rust changes (cargo test skipped — UI-only). i18n 473→474 keys, parity holds across en/he/ar/ru. Debug build BUILD_EXIT 0. No version bump — batches into v0.2.7.

### 2026-06-01 — Diff/patch review pane (Phase 50, `0d86c24`)
- **Context:** Closes scan #2.4 — the big-ticket SHOULD item Yossi has been wanting since the Sprint 2/3 batches. The need is to review what Claude (or the user) has changed in a workspace's repo without leaving winmux to type `git diff` in a terminal pane.
- **Decision:** New `PaneKind::Diff` carrying an optional `DiffSource` ({ kind: "working" | "head" | "ref", git_ref? }). A backend tokio task per Diff pane polls `git diff [HEAD|<ref>]` every 800ms (`POLL_INTERVAL_MS` in `diff_pane.rs`), hashes stdout, and emits `diff-pane-updated { pane_id, diff_text, is_git_repo }` only when the hash changes. Errors (non-repo, bad ref) emit too with `is_git_repo: false` so the FE can render the "not a git repository" state. cwd+source are re-read from `state.workspaces` under a short lock per tick — that way Phase 49-B worktree re-anchors flow through without the watcher restart.
- **Lifecycle:** `diff_pane_set_source(pane_id, source)` (re)starts the watcher with the new source, persisting onto the pane node atomically. Frontend `DiffPane` calls it on `onMount` so cold-load from `workspaces.json` and new-pane creation both bootstrap the same way. `workspace_close_pane` aborts the watcher when the pane is removed (idempotent — no-op for non-Diff panes). `diff_pane_refresh` is a one-shot fetch+emit for the manual Refresh button.
- **Frontend:** New `DiffPane.tsx` parses the unified-diff format inline — no extra dependencies. The parser recognizes `diff --git`, file-header lines, `@@` hunk headers, and ` `/`+`/`-` content; produces a flat `lines` array (rendered with per-kind CSS classes) plus a `hunks` index so ↑/↓ (or j/k) scrolls to the next hunk header. Source-mode dropdown + inline "ref…" input feed `diff_pane_set_source`. The "+ diff" button in the workspace header creates new Diff panes via the existing `splitPane` plumbing (kind union extended to include "diff").
- **Schema churn:** `LayoutNode::Pane` gains an `Option<DiffSource>` `diff_source` field (`serde(default, skip_serializing_if = "Option::is_none")` so existing `workspaces.json` files round-trip byte-identical). `split_pane_in` extended from a 4-tuple to a 5-tuple; 6 other LayoutNode::Pane construct/destructure sites threaded through the new field (3× lib.rs, 2× rpc_server.rs, 1× provisioning.rs).
- **Validation:** cargo check clean. cargo test 44 passed (41 + 3 new — the diff_pane module has 2 tokio integration tests against a real seeded git repo via tempfile + 1 existing test count drift). tsc clean. Background debug build BUILD_EXIT 0 (pending verify after this entry lands). i18n 464→473 keys × 4 locales, parity holds. No version bump — batches into v0.2.7.
- **Inventory impact:** scan #2.4 closes. SHOULD remaining = 1 (#3.1 lib.rs split). Master inventory status line updated.

### 2026-06-01 — Sprint 3 batch: drag-drop, worktrees, auto-destroy, quadrant splits, BiDi 33A expansion, CHANGELOG habit (Phase 49, `961b927`..`e47a08a`)
- **Context:** Yossi: "run everything" — single Phase 49 covering 6 of the remaining inventory items in one go (same pattern as Phase 48). Items A-F; one sub-commit each. No version bump — batches into v0.2.7.
- **Decision (6 of 6 shipped, sub-commits):**
  - **49-F** (`961b927`) — docs only: `CONTRIBUTING.md` gains a "Postmortem-style fix notes" section codifying the SYMPTOM → DISCOVERY → ROOT CAUSE → FIX format for fix-commits (with a full Phase 39.D ETXTBSY example). ~5 min/commit; fixes only, not features/refactors.
  - **49-D** (`3259aa1`) — Ctrl+Alt+I/O/K/L → top-left/top-right/bottom-left/bottom-right quadrant via two chained `splitOrMove` calls (vertical first, then horizontal in a `setTimeout(…, 0)` so the first hop's layout lands before the second reads it).
  - **49-E** (`419dc04`) — BiDi 33A expansion: `<TechText>` wraps the editor-path span in `FileEditor.tsx` and both fm-name spans in `FileManagerPane.tsx` (local + remote columns).
  - **49-C** (`56d0f79`) — auto-destroy empty workspaces. New `Settings.auto_destroy_empty_workspaces_days: Option<u32>` (default None = off; UI clamps 1-90, blank = disable). New `Workspace.last_active_at: u64` (serde default for back-compat); stamped by `workspace_set_active`. Startup sweep iterates workspaces and deletes any that are `layout.is_none()` AND `last_active_at > 0` AND `age > ttl_secs`. The `> 0` guard means never-touched workspaces survive the first run after upgrade.
  - **49-B** (`3253e0e`) — worktree-aware workspaces. New `Workspace.git_worktree: Option<PathBuf>` (serde skip_if_none, no churn on existing JSON). New `workspace_create_worktree(workspace_id, branch_name, base_branch)` tauri command runs `git worktree add <config_dir>/worktrees/<ws_id>-<branch> -b <branch> <base>` from the workspace's cwd (separate `Command::arg` calls per Absolute Rule #3; branch name allow-listed for the dir component). On success rewrites `Workspace.cwd` to the worktree path so future panes spawn inside it. UI: edit-mode block in `CreateWorkspaceModal` (local workspaces only); Sidebar shows a 🌿 chip with the path in title.
  - **49-A** (`e47a08a`) — drag-drop into terminal. New `pane_upload_dropped(workspace_id, pane_id, local_path, file_name)` in `file_manager.rs` reuses the existing SSH SFTP session, best-effort mkdirs `~/winmux-drops/`, uploads, returns `~/winmux-drops/<basename>`. `PaneView` subscribes to `getCurrentWebview().onDragDropEvent`, hit-tests the cursor position (physical px / DPR) against its own `getBoundingClientRect()` so multi-pane layouts route the drop correctly. SSH panes call the new command per file; local panes pass through. A second HTML5 `onDrop` on the pane div catches text/URL drags (browser tab URLs) since Tauri's OS-level event is files-only. All strings are POSIX single-quoted (`'foo'\''bar'`) and written via `invoke('pty_write', { sessionId: ti.sessionId, ... })` + trailing SPACE.
- **Deferred (4, all pre-spec'd):** 2.4 Diff pane, 3.1 lib.rs split, BiDi 33B (PTY-stream FSI/PDI), Full LLM control B1.
- **Validation:** cargo check clean, 41 Rust tests pass across all 6 sub-commits, tsc clean throughout, background debug build BUILD_EXIT 0 (~14.7s incremental). i18n key totals 461→464 (parity holds across en/he/ar/ru) after the three new namespaces (`ws.worktree.*`, `settings.autoDestroy.*`, `pane.drop.*`).
- **Inventory items closed:** scan #2.3 (drag-drop), #4.3 (auto-destroy empty workspaces), #4.5 (git worktree per workspace), #4.6 (quadrant split keymap), #4.7 (CHANGELOG postmortem habit). Status line below updated accordingly.

### 2026-05-31 — Sprint 2 batch: BiDi 33A, /doctor, stall telemetry, Ctrl+Alt+Arrow, PATH installer, ADR + threat model, FAB cleanup (Phase 48, `5a14540`..`52facf8`)
- **Context:** Yossi greenlit "run on the whole list together" — a single Phase 48 covering multiple SHOULD/COULD items from the competitive-scan inventory plus deferred BiDi 33A and PATH installer.
- **Decision (6 of 7 shipped, 1 deferred, sub-commits):**
  - **48-G** (`5a14540`) — `docs/ADR.md` (7 entries: Tauri/Solid/russh/named-pipe/ureq/DPAPI/per-workspace-identity) + `docs/SECURITY.md` (threat model: assets, adversaries A1-A5, per-adversary mitigations, explicit out-of-scope, open security questions).
  - **48-D** (`b224da0`) — frontend stall instrumentation: 100ms heartbeat + `PerformanceObserver({longtask})` → new `diag_log` tauri command writes to debug.log with `[ui]` prefix.
  - **48-C** (`89cb820`) — `/doctor` snapshot shared across tauri command, RPC method `"doctor"`, and `winmux doctor` CLI subcommand. Includes version, workspace + SSH counts, PTY count, RPC pipe/pool/handler-counter, bundled Linux CLI sha256, last 10 ERROR/WARN log lines. Lifted Phase 44 `HANDLER_SEQ` to `pub(crate)`. Run-Doctor button in SettingsModal Logs tab.
  - **48-A** (`f092d0c`) — BiDi 33A: `<TechText>` SolidJS component detects technical tokens (ALL_CAPS, paths, URLs, SHAs, common git ref words) and wraps each in `<code class="tech-token"><bdi>...</bdi></code>`. Applied at the 3 highest-visibility sites (Feed titles, Sidebar workspace names, PaneView pane titles). xterm.js NOT touched — 33B stays Open.
  - **48-E** (`ec58541`) — Ctrl+Alt+Arrow split-or-move: layout tree walk finds nearest pane in the requested direction; focuses it if found, else `splitPane(current, horizontal|vertical)`.
  - **48-B** (`1e28af5` + fixup `52facf8`) — NSIS installer hooks for HKCU PATH registration. First attempt used `${StrLoc}` standalone (broke NSIS compile with "STRFUNC_CALL_StrLoc requires 5 parameter(s)"); fixup dropped StrFunc, uses only `WordReplace` from WordFunc.nsh with the standard `!insertmacro` activation. WiX MSI Environment element NOT done — needs a separate fragment file + WiX-side testing (filed for a follow-up; MSI installers won't auto-PATH).
- **Deferred (1):**
  - **48-F** — per-workspace browser session dir. The browser pane is iframe-based (`HTMLIFrameElement`), not a separate Tauri WebView, so `additional_browser_args --user-data-dir=...` has no plumbing point with the current architecture. A real fix needs browser-pane-as-WebView (architecture-level change) as its own phase.
- **Validation:** cargo check clean, 41 Rust tests pass, tsc clean across all sub-commits; debug build BUILD_EXIT 0 after the 48-B fixup. app.exe ~31.1 MB. No version bump — batches into v0.2.7.
- **Open→Decided moves performed alongside this entry:** "BiDi mixed-content rendering" replaced with a 33B-only Open entry; "PATH auto-registration" removed (now Decided in 48-B); ADR + threat model docs were #3.3 of the master inventory, status line updated.

### 2026-05-31 — Headless connect now bootstraps the reverse tunnel (Phase 47.A, `7c38594`)
- **Context:** Phase 47 closed most of the "stuck searching" report but left a known limitation: a workspace whose detection toggle was on but with no terminal pane ever opened couldn't bootstrap the port-watcher. Phase 41's headless connect set up SSH auth and stored a `Session::Ssh`, but never called `tcpip_forward` — so `workspace_tunnel_tokens[ws]` + `internal_reverse_tunnel_remote_ports[ws]` stayed empty and `try_ensure_port_watcher` bailed with "needs a pane to bootstrap." Yossi's actual usage pattern is: open a workspace and look at ports without necessarily opening any pane — so the limitation was painful in practice.
- **Decision:** Extracted `setup_workspace_reverse_tunnel(state, &mut handle, ws, &token) → u16` from spawn_ssh's inline block. Does the workspace-level slice only: `tcpip_forward` → record the kernel-assigned remote port in `internal_reverse_tunnel_remote_ports`, stash the token in `workspace_tunnel_tokens`, fire `spawn_port_watcher` (deduped). The pane-specific bits — `write_remote_env_file(&pane_id)` and the `WINMUX_PANE_ID` `set_env` on the shell channel — stay in `spawn_ssh`. `workspace_ensure_connected` now (a) captures `SshHandshake.tunnel_token` instead of dropping it, (b) keeps `handle` mutable, and (c) calls the helper before moving the handle into `Arc` — short idempotency pre-check, async tunnel setup, then re-check + insert under the lock; a pane racing in mid-tunnel-setup drops the spare handle (and its tunnel with it).
- **Compile detour:** First attempt took `&Handle`; `tcpip_forward` requires `&mut self` (E0596). Switched the helper to `&mut Handle`, which forced calling it before the Arc-wrap in `ensure_connected` (Arc only gives shared access).
- **Outcome / Commit:** `7c38594` (debug build BUILD_EXIT 0, app.exe ~31.0 MB). 41 Rust tests pass; tsc clean. Single backend file changed (lib.rs +111/-50). No version bump — batches into v0.2.6.

### 2026-05-31 — Port-watcher lifecycle across workspaces + FAB cleanup (Phase 47 `e2ae06d` + 47.E `0e0a4de`)
- **Context:** Yossi reported workspace B "stuck searching" after switching from A. Root causes: (1) `winmux port-watch` was only spawned inside `spawn_ssh` (pane path) — B without a pane had no watcher; (2) toggle off only suppressed dispatch instead of stopping the watcher; (3) no replay endpoint so the FE's `detectedPorts` couldn't bootstrap on workspace switch. Separately, two floating Notes/Settings FAB buttons remained in the workspace area despite the Phase 39 sidebar consolidation.
- **Decision (Phase 47):** Extracted `spawn_port_watcher(state, handle, ws, remote_port, token)` from spawn_ssh's inline block — same path now reachable from anywhere. `AppState` gains `port_watcher_tasks: HashMap<String, JoinHandle>` (so `.abort()` on toggle-off works) and `workspace_tunnel_tokens` (spawn_ssh stashes the per-workspace token so a later ensure call can dial back). New `try_ensure_port_watcher` helper looks up handle + tunnel port + token; spawns watcher if all present, else dlogs "needs a pane to bootstrap." New tauri commands: `workspace_ensure_port_watcher` (fired by the App.tsx activation effect) and `list_detected_ports` (snapshot replay on workspace switch). `workspace_set_auto_port_forward` now actually starts/stops the watcher: `true` → ensure; `false` → `clear_workspace_detection` (abort task + clear `detected_ports[ws]` + emit `port-detection-cleared`). FE listens for the new event and re-fires the ensure+snapshot when ws.id OR ws.auto_port_forward changes. Address display (0.0.0.0 vs 127.0.0.1 vs ::) preserved per Yossi.
- **Decision (47.E):** Removed `<button class="notes-fab">` and `<button class="settings-fab">` from App.tsx + their CSS rules + RTL mirrors + unused `fab.notes`/`fab.settings` i18n keys (parity re-verified at 452 × 4 locales). Ctrl+Shift+N for Notes is wired separately and stays.
- **Known limitation:** for a workspace whose toggle is on but no pane has ever connected this session, the headless Phase 41 connect doesn't set up a reverse tunnel — `try_ensure_port_watcher` dlogs "open a pane to bootstrap" and returns. Phase 47.A will extract the tunnel setup from spawn_ssh into a helper that the headless path also calls, so detection bootstraps without requiring a pane.
- **Outcome / Commits:** `e2ae06d` (Phase 47, debug build BUILD_EXIT 0) + `0e0a4de` (47.E, debug build BUILD_EXIT 0, app.exe ~31.0 MB). 41 Rust tests pass; tsc clean. No version bump — batches into v0.2.6.

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
