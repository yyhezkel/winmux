# Phase 69 — Server-side Claude Chat for Mobile (DESIGN)

> Status: **69.A–D BUILT** on `69-claude-chat` (approved 2026-06-30, not
> pushed). This doc is the contract; two deviations during the build:
> (1) the daemon code stayed **flat `package main`** rather than
> `internal/claude/` — a split package would create a WS↔session↔RPC import
> cycle on a single-binary daemon, and flat matches the existing 6-file
> daemon; (2) the 69.C hook round-trip is proven with a **Go client**
> replicating the CLI wire format — a real `winmux claude-hook` round-trip
> should run on Linux at 67.C integration. See `docs/DECISIONS.md`.
> Branch: `69-claude-chat`. Nothing pushed to `main`.

## 1. Motivation & model

Yossi's call: **mobile is a Claude *chat client*, not a mirror of the
desktop.** The phone does not render a terminal and does not attach to a
desktop pane. Instead it talks to the **workspace daemon** (the Phase 68
`winmux-insights` Go binary, extended here) which spawns its *own*
independent Claude Code sessions and streams structured chat events back.

Consequences of the model:

- The desktop is **unchanged**. Desktop Claude panes keep using the
  Phase 66 hook → desktop-RPC path exactly as today.
- Mobile gets a **rich chat UI** (assistant turns, tool calls, tool
  results, permission prompts) driven by `claude --output-format
  stream-json`, *not* by scraping a PTY.
- The daemon already exists, already listens on localhost, already does
  bearer-token auth, already cross-compiles to linux x64/arm64, and is
  already installed/managed by the Add-on framework. Phase 69 = **new
  endpoints on that daemon**, not a new component.

```
┌─ Mobile app (chat UI) ─────────────────────────────────────────────┐
│   message list · tool cards · permission prompts · model picker     │
└───────────────┬─────────────────────────────────────────────────────┘
                │ REST (session CRUD) + WebSocket (live chat)
                │ bearer token, per-device, workspace-scoped
                │ [transport to localhost:7879 = Phase 67.C — see §9]
┌───────────────▼─────────────────────────────────────────────────────┐
│ workspace server : winmux-insights daemon (extended)                 │
│                                                                      │
│   internal/claude/  SessionManager ── spawns ──► claude CLI          │
│                     StreamParser    ◄── stdout ─ (stream-json)       │
│                                      ── stdin ──► (stream-json in)    │
│                     (per session: replay ring buffer + status)       │
│                                                                      │
│   internal/transport/ws.go   gorilla/websocket fan-out               │
│                                                                      │
│   RPC server (Phase 66 wire format) ◄── claude-hook ── CLI           │
│        └── bridges hook permission_request ⇄ mobile WS               │
│                                                                      │
│   SQLite: sessions, devices, replay  (alongside metrics.db)          │
└──────────────────────────────────────────────────────────────────────┘
```

Key reuse:
- **Auth & token plumbing** — same bearer model as `/current` etc.
- **Hook wire format** — the daemon becomes a *second RPC server* speaking
  the Phase 66 `feed.push` protocol, so hooks from mobile-spawned Claude
  reach the phone with zero changes to the CLI or `hooks/claude-code.json`.
- **Install/lifecycle** — ships as a daemon version bump (1.0.1 → 1.1.0);
  the Add-on framework's update path already restarts the systemd unit.

## 2. Scope boundary (what Phase 69 is NOT)

- **Not** the mobile app itself (that's 67.C / a separate repo).
- **Not** the network path from phone → server. Phase 69 makes the daemon
  *speak* the chat protocol on `127.0.0.1:7879`; **how a phone reaches that
  localhost port** (winmux tunnel relay, a mobile SSH local-forward, or a
  cloud relay) is **Phase 67.C**. For Phase 69 dev/testing we forward 7879
  over SSH exactly as the desktop Monitor already does. This keeps the
  daemon localhost-only and inbound-exposure-free (same security posture as
  Phase 68). See §9-Q4.
- **Not** desktop behaviour. No desktop file changes in Phase 69.

## 3. 69.A — Claude session management

### 3.1 Spawn model — direct pipes, not a tmux PTY

The user sketch said "tmux + claude". On reflection there's a tension:
`--output-format stream-json` is a **pipe protocol** (newline-delimited
JSON on stdout; user turns fed as JSON on stdin with
`--input-format stream-json`). A tmux PTY gives a *terminal*, which is the
mirror model we explicitly rejected — you'd be scraping a TUI, not reading
clean events.

**Recommendation: spawn Claude directly via `os/exec` with stdin/stdout
pipes (no tmux).** Persistence is handled by two things that fit the chat
model better than tmux:

1. **The daemon is the durable host** — it runs under
   `systemd --user` with `Restart=on-failure`, holds the child process,
   and survives mobile disconnects. A phone dropping its WebSocket does
   **not** kill Claude (see §9-Q2).
2. **Claude Code's own `--resume`** — conversation state is persisted by
   Claude under `~/.claude`. If the *daemon itself* restarts (kills its
   children), the session row is marked `interrupted`; the next user turn
   re-spawns with `claude --resume <claude_session_id>` and continues the
   same conversation. This is real durability without a PTY.

Spawn command (arg-array, Rule #3 — never string-concat):

```go
exec.Command("claude",
    "--output-format", "stream-json",
    "--input-format",  "stream-json",
    "--verbose",                       // required for stream-json
    "--include-partial-messages",      // token-level streaming (optional, toggle)
    "--model", model,                  // if provided
    "--append-system-prompt", sysPrompt, // if provided
    // cwd via cmd.Dir; never via shell cd
)
```

`cmd.Dir = cwd`. Env carries the **session RPC token** + socket addr so
hooks bridge back to the daemon (see §5). `--permission-mode` is left at
default so the Phase 66 hooks fire (that's how mobile gets approval
prompts).

> **Decision to confirm (§9-Q1):** direct-pipes (recommended) vs.
> tmux-wrapped. If Yossi wants `tmux ls` visibility / desktop attach for
> debugging, we can wrap in `tmux new-session -d` *and still* pipe
> stream-json through a FIFO pair — but that adds complexity for little
> gain in the chat model. Default to pipes.

### 3.2 REST endpoints

All under the existing bearer-auth mux; all JSON; all localhost-bound.

```
POST   /api/claude/session
  Body: { cwd?, model?, system_prompt? }
  Resp: { session_id, claude_session_id?, status: "starting" }
  Action: allocate session row, spawn claude (§3.1), return immediately.
          claude_session_id is filled once the CLI emits its init event.

GET    /api/claude/sessions
  Resp: { sessions: [ SessionSummary ] }

GET    /api/claude/session/{id}
  Resp: SessionSummary
        { id, status, started_at, model, cwd, message_count,
          last_activity_at, pending_tool?, pending_hook? }

DELETE /api/claude/session/{id}
  Action: send SIGINT, then SIGTERM; mark status=killed; close WS.
```

`status` ∈ `starting | active | waiting_input | waiting_hook | interrupted
| stopped | killed | error`. The `workspace_id` from the body sketch is
dropped — a daemon **is** one workspace; the session is implicitly scoped
to it.

### 3.3 WebSocket

```
GET /ws/claude/session/{id}      (Upgrade; bearer token in query or header)
```

On connect the daemon **replays** the session's buffered events (so a
reconnecting phone rebuilds the transcript) then switches to live tail.

**Server → client** (mirrors Claude stream-json, normalized):

```jsonc
{ "type": "session_init", "claude_session_id": "...", "model": "..." }
{ "type": "assistant", "text": "...", "partial": false }
{ "type": "assistant_delta", "text": "..." }          // if partial-messages on
{ "type": "tool_use", "id": "toolu_..", "name": "Bash", "input": {...} }
{ "type": "tool_result", "tool_use_id": "toolu_..", "content": "...", "is_error": false }
{ "type": "hook_request", "req_id": "req_..", "tool_name": "Bash",
  "tool_input": {...}, "title": "Run `ls -la`?", "decision_required": true }
{ "type": "result", "subtype": "success", "usage": {...} }   // turn done
{ "type": "status", "status": "waiting_input" }
{ "type": "error", "message": "..." }
```

**Client → server**:

```jsonc
{ "type": "user_input", "content": "fix the failing test" }
{ "type": "hook_decision", "req_id": "req_..", "decision": "allow" }  // or "deny"
{ "type": "interrupt" }       // Ctrl-C equivalent → SIGINT to claude
{ "type": "stop_session" }    // graceful end (same as DELETE)
```

A `user_input` is translated to a stream-json user message written to
Claude's stdin. Multiple phones on one session share the same WS fan-out
(broadcast); inputs are serialized.

## 4. 69.B — stream-json parser (Go)

New `internal/claude/parser.go`. Reads Claude stdout line-by-line, each
line one JSON object with a top-level `type`. Claude Code's stream-json
events we handle:

| Claude event (`type`)        | Daemon action                                   |
|------------------------------|-------------------------------------------------|
| `system` (subtype `init`)    | capture `session_id`, `model` → emit `session_init` |
| `assistant`                  | extract text + `tool_use` blocks → emit `assistant` / `tool_use`, set `pending_tool` |
| `user` (tool_result)         | emit `tool_result`, clear matching `pending_tool` |
| `stream_event` (partial)     | emit `assistant_delta` (only if partial on)     |
| `result`                     | emit `result`, set status `waiting_input`       |

Parser rules:
- **Never block** the stdout reader on a slow WS — events go to the
  per-session replay buffer first, then fan out. Backpressure is bounded
  by the ring buffer (drop-oldest with a logged marker, like insights' DB
  sweep).
- **Tolerant**: an unrecognized `type` is forwarded verbatim under
  `{ "type": "raw", "event": <obj> }` so the phone (and we) can evolve
  without a daemon redeploy. Malformed/non-JSON lines are logged
  (metadata only, Rule #1 — never the content) and skipped.
- Tool input/result payloads can be large → truncate in the *summary*
  fields but stream full content on the WS (the phone decides how to
  render). Replay buffer caps total bytes per session.

**Rule #1 caveat:** stream-json carries the user's actual prompts and
Claude's output. That content flows over the WS to the phone (that's the
product) but is **never** written to `insights.log` or `debug.log`. Logs
get counts and event types only.

## 5. 69.C — hook integration (the crux)

This is what makes mobile approval work, and it reuses Phase 66 wholesale.

### 5.1 How Phase 66 works today (desktop)

Claude fires a `PreToolUse` hook → runs `${WINMUX_BIN} claude-hook
pre-tool-use` → the CLI loads `~/.winmux/run/last.env`, reads
`WINMUX_SOCKET_ADDR` + `WINMUX_TUNNEL_TOKEN` + `WINMUX_PANE_ID`, opens an
RPC connection, does the **HMAC challenge-response handshake**, and pushes
a `permission_request` (`{request_id, kind, subkind, pane_id, payload,
wait_timeout_seconds}`). The **desktop** evaluates policy (auto/gate/block),
shows a card if needed, and replies `{request_id, decision}`. The CLI turns
that into Claude Code's `{hookSpecificOutput:{permissionDecision}}`.

### 5.2 Phase 69 — point that RPC at the daemon

When the **daemon** spawns Claude (§3.1), it injects into that process's
environment:

```
WINMUX_SOCKET_ADDR = 127.0.0.1:<rpc_port>     # daemon's own RPC listener
WINMUX_TUNNEL_TOKEN = <per-session HMAC token> # daemon-generated, never sent to phone
WINMUX_PANE_ID      = mob_<session_id>          # synthetic pane id for this session
```

So when Claude fires a hook **inside a mobile session**, the existing CLI
hook code connects to the **daemon** (not the desktop), passes the same
HMAC handshake (daemon holds the per-session token), and pushes the same
`permission_request`. The daemon:

1. Maps `pane_id = mob_<session_id>` → the live session.
2. Applies the session's **policy** (default = gate everything, i.e. ask
   the phone; optionally an auto/block policy per device — see §6). The
   daemon can reuse the `winmux-policy` semantics conceptually, but since
   it's Go, round 1 implements the simple 3-state check directly.
3. If gate: emits `hook_request` over the WS, parks the RPC call, and waits
   (up to `wait_timeout_seconds`, clamped [1,600]) for the phone's
   `hook_decision`.
4. Replies `{request_id, decision}` on the RPC socket → CLI → Claude.
5. On timeout / no phone connected: falls back to the **static policy**
   (auto/gate → `deny` for safety on mobile, or `allow` if Yossi prefers
   convenience — §9-Q3), logged.

**Net effect:** zero changes to `hooks/claude-code.json` or the CLI. The
daemon is just another endpoint that speaks the Phase 66 RPC dialect. This
is the single most important reuse in Phase 69 — we implement the *server*
half of the existing protocol in Go.

> **Build note:** the Go daemon must implement the HMAC challenge-response
> exactly (`WINMUX-CHALLENGE <nonce>` → `WINMUX-RESPONSE <hmac>` →
> `WINMUX-OK|DENIED`) and the length-framed `feed.push` JSON. We port this
> from `cli/src/main.rs:894-960` + `rpc_server.rs`. A round-trip
> integration test (real `winmux claude-hook` against the Go RPC server) is
> a required acceptance gate.

### 5.3 Why not "WS only, skip hooks"?

stream-json *does* surface permission decisions via Claude's control
protocol (`can_use_tool`), but that path is newer, less documented, and
version-coupled. Hooks are the mechanism winmux already owns end-to-end and
already ships to every server. **Reuse hooks.** (If a future Claude drops
hook support we revisit; noted as a risk.)

## 6. 69.D — auth, routing, rate limiting

Two distinct tokens, deliberately separate:

| Token | Audience | Lifetime | Stored |
|-------|----------|----------|--------|
| **Device token** (bearer) | phone → daemon REST/WS | long-lived, revocable | SQLite `devices`, hash-at-rest |
| **Session RPC token** (HMAC) | CLI hook → daemon RPC | per session, ephemeral | in-memory only |

- The insights bearer token (`~/.winmux/insights/token`) stays for the
  Monitor endpoints. Mobile gets **its own** device tokens so they can be
  revoked per-phone without breaking the desktop Monitor. A device token is
  minted by the desktop (during 67.C pairing) and registered with the
  daemon; round 1 we can seed one manually for dev.
- **Workspace-scoped**: a device token is valid only on the daemon that
  registered it (a daemon = one workspace), so scoping is structural.
- **Rate limiting**: max **50 active sessions per device** (configurable),
  enforced at `POST /api/claude/session` against the `sessions` table.
  Also a global daemon cap (e.g. 100) to bound memory. Over-limit → HTTP
  429 with a clear body.
- Session RPC tokens are generated with crypto/rand (32 bytes hex), never
  logged (Rule #8 treats them like the tunnel token), never sent to the
  phone.

## 7. Data model (SQLite, alongside metrics.db)

A separate `chat.db` (keeps metrics retention sweeps independent):

```sql
CREATE TABLE devices (
  id          TEXT PRIMARY KEY,      -- device id
  token_hash  TEXT NOT NULL,         -- sha256 of the bearer token
  label       TEXT,
  created_at  INTEGER,
  revoked_at  INTEGER
);

CREATE TABLE sessions (
  id                TEXT PRIMARY KEY,  -- "mob_<rand>"
  device_id         TEXT,
  claude_session_id TEXT,             -- from Claude's init event (for --resume)
  cwd               TEXT,
  model             TEXT,
  status            TEXT,             -- starting|active|waiting_*|interrupted|stopped|killed|error
  policy            TEXT,             -- auto|gate|block (default gate)
  started_at        INTEGER,
  last_activity_at  INTEGER,
  message_count     INTEGER DEFAULT 0
);
CREATE INDEX idx_sessions_device ON sessions(device_id, status);

-- bounded replay buffer for reconnect (drop-oldest per session)
CREATE TABLE replay (
  session_id TEXT, seq INTEGER, ts INTEGER, event TEXT,  -- event = normalized WS JSON
  PRIMARY KEY (session_id, seq)
);
```

Replay is capped (e.g. last 500 events or 1 MB per session, whichever
first). On `DELETE`/cleanup the rows are purged.

## 8. Security considerations

- Daemon stays **localhost-only**; no new inbound ports. Mobile transport
  is Phase 67.C's authenticated channel (§2).
- Two-token split (§6): a leaked device token can't impersonate hook
  decisions; a session HMAC token can't call the REST API.
- Hook decisions are **deny-biased on timeout** by default (§9-Q3) — a
  silent/absent phone must not auto-approve a `Bash rm -rf`.
- Rule #1: prompt/response content streams to the phone but is **never**
  logged. Rule #3: claude/tmux spawned via arg-arrays, cwd via `cmd.Dir`,
  no shell concat. Rule #8: HMAC/device tokens never logged.
- `DELETE`/`interrupt` are the only process-control surfaces; both
  token-gated and session-scoped.
- The daemon never execs arbitrary strings from the phone — `user_input`
  is delivered to Claude as a stream-json **message**, not a shell command.

## 9. Open questions — recommendations for Yossi

**Q1 — Spawn: direct pipes vs tmux?**
*Recommend direct `os/exec` pipes (no tmux)* — stream-json is a pipe
protocol; tmux reintroduces the PTY/mirror we rejected. Persistence comes
from the systemd-managed daemon + Claude `--resume`. (Your three explicit
questions below.)

**Q2 — Session cleanup when mobile disconnects?**
*Recommend: session survives disconnect.* The daemon owns the process; a
dropped WS just stops the fan-out. Reconnect replays the buffer and
resumes. A **sweeper** kills sessions that are both client-less **and**
idle for `cleanup_idle_hours` (default **24h**); `DELETE` kills now. This
mirrors tmux persistence semantics without tmux.

**Q3 — Desktop and mobile both open Claude in the same workspace?**
*Recommend: separate, independent sessions.* Desktop Claude runs in its
pane's tmux session; mobile Claude is a daemon-owned process with its own
conversation. They do **not** share state — unifying them *is* the mirror
model you rejected. (Optional v2: the daemon can *list* desktop
`winmux-*` tmux sessions read-only as "also running", but never drive
them.)

**Q4 — Mobile→daemon transport** (the real networking question): tunnel
relay vs mobile-SSH-forward vs cloud relay? **Deferred to Phase 67.C** by
your plan; Phase 69 assumes localhost reachability and forwards 7879 over
SSH for dev. Flagging so it's not forgotten.

**Q5 — Hook timeout fallback on mobile: deny or allow?**
*Recommend deny* (safety: an absent phone shouldn't approve destructive
tools). Desktop's static fallback is allow-for-auto/gate; mobile is
higher-risk (often unattended), so default deny. Your call.

**Q6 — Daemon version bump** 1.0.1 → **1.1.0** (additive endpoints). The
Add-on update path already restarts the unit; existing Monitor endpoints
unchanged. OK?

**Q7 — `gorilla/websocket` dependency** — adds one CGO-free dep, fine for
the existing x64/arm64 cross-compile. OK? (Alternative: stdlib
`golang.org/x/net/websocket`, but gorilla is the standard.)

## 10. Suggested build order (matches your priorities)

1. **69.A** — `internal/claude/` SessionManager + spawn (pipes) + REST
   CRUD + a raw WS that forwards *unparsed* stdout. Prove spawn/teardown +
   rate limit + replay skeleton. (~3d)
2. **69.B** — `parser.go` + normalized WS events + `user_input` /
   `interrupt` on stdin. Chat works without approvals. (~2d)
3. **69.C** — Go RPC server speaking the Phase 66 dialect (HMAC handshake +
   `feed.push`), inject session env, bridge `hook_request`/`hook_decision`.
   Acceptance: real `winmux claude-hook` round-trips. (~2d)
4. **69.D** — device-token table, workspace scoping, 50/device rate limit,
   timeout fallback policy. (~1d)

Each step is independently testable on the `69-claude-chat` branch with the
daemon run locally + a `websocat`/`curl` harness; nothing touches `main` or
the desktop until the 67.C integration step.

---
*After 69 lands: integrate with the mobile session work (67.C) — the phone
speaks this protocol, and we validate the end-to-end pairing + transport
(§9-Q4) there.*
