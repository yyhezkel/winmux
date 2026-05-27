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
- **Status:** Going through together with Yossi to triage MUST / SHOULD / COULD / SKIP. As individual items are decided, move them to their own Decided / Open entries with phase + commit references. Master inventory stays here until fully triaged.
- **Also flagged:** the `winmux` name is taken by 8 projects on GitHub — rebrand caveat (see scan doc's "Naming Caveat" section).

### 2026-05-27 — MCP server in browser
- **Context:** User wants winmux's MCP server reachable from the browser tab.
- **Options:**
  - A. Auto-register `winmux-mcp` in claude's local MCP config on install
  - B. Remote bridge over SSH tunnel
- **Status:** Awaiting user choice. Raised mid-session, deferred multiple times.

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

### 2026-05-27 — winget submission
- **Context:** Publish winmux to `winget-pkgs` for corporate-machine compatibility and CLI install/upgrade.
- **Status:** Deferred — user said "wait with winget" after v0.2.3 ruled it not urgent.

### 2026-05-27 — README "Shipped" refresh
- **Context:** README "Shipped in v0.1.0" line is two major releases stale (Phases 16–34 unrostered).
- **Status:** Open, ~30 min refresh task.

### Long-standing — Roadmap items carried over from v0.1.0 README
- PATH auto-registration in WiX installer
- Code-signing for MSI / NSIS (SmartScreen warning today)
- Auto-update via signed manifest + delta downloads (currently unsigned, full-download)
- ARM64 Windows build
- aarch64-linux CLI
- Scoop manifest

---

## Decided

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
