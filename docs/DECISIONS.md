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

### 2026-05-27 — Logs accessibility for end users
- **Context:** `dlog()` writes to `%APPDATA%\winmux\debug.log` but no UI exposes the path. Non-technical users can't find logs.
- **Proposal:** Settings → Logs row with "Open logs folder" + "Copy log path" buttons (uses existing `tauri-plugin-opener`). ~30 lines.
- **Status:** Awaiting user confirmation.

### 2026-05-27 — README "Shipped" refresh
- **Context:** README "Shipped in v0.1.0" line is two major releases stale (Phases 16–34 unrostered).
- **Status:** Open, ~30 min refresh task.

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
