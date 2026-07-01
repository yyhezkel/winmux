# Phase 77 — `winmux-server` as a First-Class Component

> **STATUS: APPROVED — in implementation (Sprint 1).** Yossi approved the design
> and chose to enter S1 directly, updating this doc in-flight. Renamed from
> `PHASE-74-DESIGN.md` (74 was taken by split-QR pairing, commit `2ebb645`).

## 0. Resolved decisions (Yossi's answers)
- **Q0 — Number:** **Phase 77** (74 taken). ✓
- **Q1 — Location:** inside the winmux repo at **`app/src-tauri/server/`**. ✓
- **Q3 — Workspace shared state:** = **active sessions + subscribers-per-session
  + pending requests.** Two use-cases: **8a Multi-client attach** (several
  clients on the *same* Claude session see the same chat / tool-use progression
  / hook requests) and **8b Notification broadcast** (a client gets a "Claude
  needs input" notification and can answer — approve/deny hook, send message —
  the answer reaches Claude and every other subscribed client sees it). ✓
- **Q4 — OpenAPI framework:** implementer's choice; **`huma` recommended**. The
  framework decision is **deferred to S4** — S1 keeps stdlib `net/http` behind
  the `api` package so it doesn't block the module split. ✓
- **Q5 — `workspace_id` authority:** **server-authoritative UUID**, minted at
  workspace creation, handed to mobile at pairing/redeem; mobile never invents
  one. ✓
- **Mobile contract:** `docs/PHASE-77-MOBILE-API-EXPECTATIONS.md` is expected
  from the mobile session (not in the repo as of S1 start) — fold its
  requirements in as they land. See §4.4, §8, §14.

> **⚠ Streaming-contract risk (from the mobile session — matters in S1):**
> **OpenAPI does not describe WebSocket frames.** The real risk is the **WS
> frame contract**, not the REST framework choice. S1 pins the streaming frame
> schemas up front — see **§4.4 Streaming contract**.

---

## 1. Motivation

The metrics daemon `winmux-insights` has quietly grown from a CPU/RAM sampler
(Phase 68) into a real multi-subsystem server: metrics, **Claude chat sessions**
(Phase 69), **mobile pairing** (Phase 70/74-QR), a **hook policy bridge** (Phase
66/69.C), and **process hygiene** (Phase 76). It is one flat `package main`
(21 `.go` files) with an ad-hoc route scheme (`/current` next to
`/api/claude/...`), a single shared bearer token, and no contract that clients
can generate against.

Desktop and (future) mobile both talk to it, but there is **no stable API
surface** — every client hand-writes request/response types that can silently
drift from the Go structs. As mobile lands, this becomes a correctness and
velocity risk.

**Phase 77 makes the server a first-class component**: clean internal module
boundaries, a normalized versioned HTTP/WS API, an auto-generated OpenAPI spec,
and SDKs (Kotlin + TypeScript) generated from that spec so desktop and mobile
share one source of truth.

### Goals
- One server, **clean module boundaries**, no import cycles.
- **Normalized, versioned API** (`/api/v2/...`) with request/response schemas.
- **Auto-generated OpenAPI** → generated **Kotlin + TypeScript SDKs**, version-locked.
- **Per-client auth scoping** (desktop vs. each paired mobile device).
- **Zero-downtime migration** from `winmux-insights` 1.x with data preserved.
- Monorepo — everything stays in the `winmux` repo.

### Non-goals (explicitly deferred)
- **Splitting into separate repos.** Only if/when iOS lands or the overhead
  justifies it. Monorepo now.
- Rewriting the desktop's Rust ↔ server transport. The desktop keeps talking
  over the existing reverse SSH tunnel + `insights_fetch`/`daemon_curl`; only
  the *paths + payloads* it hits are normalized.
- A general plugin system. The subsystems are known and finite.
- Multi-tenancy / multi-user on one server. One Linux user, one server.

---

## 2. Current state (what exists today, v1.2.7)

```
app/src-tauri/insights/           module winmux-insights, package main (flat)
  main.go        boot, flags, log rotation (Phase 75.1), goroutine wiring
  api.go         http.ServeMux, auth(), metrics handlers
  sampler.go     gopsutil CPU/RAM/disk/net + /current cache (Phase 72.3)
  docker.go      unix-socket docker probe + stats
  store.go       SQLite metrics ring (modernc, CGO-free)
  util.go        tailFile, helpers
  hygiene.go     Phase 76 process hygiene (port-watch dups, orphan claude)
  chat_*.go      Phase 69 Claude chat: session mgr, stream-json parser,
                 hook RPC bridge, pairing (device tokens), chat SQLite store
```

**Routes today** (note the inconsistency — this is what we normalize):

| Subsystem | Routes | Auth |
|---|---|---|
| liveness | `GET /healthz` | none |
| insights | `GET /current` `GET /history` `GET /processes` | bearer |
| docker | `GET /docker` `POST /docker/{id}/action` | bearer |
| logs | `GET /logs?tail=N` | bearer |
| hygiene | `GET /hygiene` `POST /hygiene/kill` | bearer |
| chat | `GET/POST /api/claude/session[s]` `WS /ws/claude/session/{id}` | bearer |
| pairing | `POST /api/pairing/issue` `POST /api/pairing/redeem` `GET/DELETE /api/pairing/devices[/{id}]` | bearer / device token |
| hooks | internal `127.0.0.1:<rand>` TCP RPC (not HTTP) | HMAC |

**Known structural constraint (important):** the daemon is deliberately *flat*.
Phase 69's DECISIONS note records that a naive `internal/claude/` split created a
**WS ↔ session ↔ hookRPC import cycle**. Any module refactor MUST solve this by
dependency inversion (interfaces in a leaf package), not by wishful directory
moves. See §4.1.

---

## 3. Target architecture

### 3.1 Module layout (monorepo, under `app/src-tauri/server/`)

```
app/src-tauri/server/                module winmux-server, v2.0.0
  cmd/winmux-server/main.go          thin: parse flags, build deps, run api.Server
  internal/
    core/          # LEAF package: shared types + interfaces ONLY (no logic,
                   # no imports of siblings) — breaks the cycle. e.g.
                   #   type Sampler interface { Current() Snapshot }
                   #   type SessionManager interface { ... }
                   #   type HookBridge interface { ... }
    config/        # persistent state: opens the single SQLite db, migrations,
                   # token store; depends only on core
    auth/          # bearer + per-client (device) scoping middleware; core
    insights/      # metrics sampler + docker + store  (impl of core.Sampler)
    chat/          # Claude sessions + stream-json + WS  (impl of core.SessionManager)
    pairing/       # QR device pairing + device tokens
    hooks/         # policy engine bridge (the 127.0.0.1 RPC listener)
    files/         # NEW: directory listing + shared-folder up/download
    logs/          # NEW: per-client log storage (list/read)
    workspace/     # NEW: cross-client shared state + event bus (WS)
    api/           # HTTP routes, version prefix, OpenAPI wiring; imports the
                   # subsystem packages THROUGH core interfaces where cycles
                   # would otherwise form
  sdk/
    kotlin/        # generated — do not hand-edit
    typescript/    # generated — do not hand-edit
  openapi.json     # generated spec (checked in for diff review + SDK gen)
```

### 3.2 Dependency direction (ASCII)

```
                         cmd/winmux-server
                                │  builds concrete impls, injects into api
                                ▼
        ┌──────────────────── api ─────────────────────┐
        │   routes + version prefix + OpenAPI + auth mw │
        └───┬───────┬───────┬───────┬───────┬───────┬───┘
            │       │       │       │       │       │        (all depend on core
            ▼       ▼       ▼       ▼       ▼       ▼         interfaces, never on
        insights  chat   pairing  hooks  files   logs  ...   each other directly)
            │       │       │       │       │       │
            └───────┴───────┴───┬───┴───────┴───────┘
                                ▼
                              core          (leaf: types + interfaces, zero deps)
                                ▲
                              config, auth   (also leaf-ish; depend on core only)
```

**Rule that keeps it acyclic:** cross-subsystem needs (chat needs the hook
bridge; workspace broadcasts to WS clients) are expressed as **`core`
interfaces**. `chat` imports `core.HookBridge`, not `hooks`. `cmd` wires the
concrete `hooks.Bridge` into `chat` at startup. No sibling imports a sibling.

### 3.3 Transport

- HTTP/1.1 + JSON for request/response.
- WebSocket for streaming (chat stream-json today; workspace events new).
- Everything stays behind the reverse SSH tunnel for desktop; behind
  nginx+TLS+device-token for mobile (Phase 70). No transport change.

---

## 4. API surface (v2)

All routes move under a **version prefix `/api/v2/`** and a consistent
subsystem segment. `GET /healthz` stays unversioned (liveness for probes).
`GET /api/version` returns `{name, version, api_versions:[2], min_client}` so a
client can negotiate.

### 4.1 Existing subsystems (renamed, behavior unchanged)

| v2 route | was | notes |
|---|---|---|
| `GET /api/v2/insights/current` | `/current` | cached snapshot (Phase 72.3) |
| `GET /api/v2/insights/history?metric=&since=` | `/history` | |
| `GET /api/v2/insights/processes?limit=` | `/processes` | |
| `GET /api/v2/insights/docker` | `/docker` | |
| `POST /api/v2/insights/docker/{id}/action` | `/docker/{id}/action` | `{cmd}` |
| `GET /api/v2/insights/hygiene` | `/hygiene` | Phase 76 |
| `POST /api/v2/insights/hygiene/kill` | `/hygiene/kill` | `{pids}` |
| `GET /api/v2/logs/daemon?tail=N` | `/logs` | server's own log (rename to avoid clash with new per-client logs) |
| `POST /api/v2/chat/sessions` … | `/api/claude/session[s]` | |
| `WS  /api/v2/chat/sessions/{id}/stream` | `/ws/claude/session/{id}` | |
| `POST /api/v2/pairing/issue` | `/api/pairing/issue` | |
| `POST /api/v2/pairing/redeem` | `/api/pairing/redeem` | Phase 74 QR-split payload v2 |
| `GET/DELETE /api/v2/pairing/devices[/{id}]` | `/api/pairing/devices` | |

### 4.2 New subsystems

**Files** (directory picker + shared folder). Root-scoped, traversal-safe.
```
GET  /api/v2/files/list?path=/abs/or/rel
     → { path, entries:[{name,is_dir,is_link,size,modified,perms}] }
POST /api/v2/files/upload         (multipart or chunked; body=bytes, ?path=)
     → { path, bytes }
GET  /api/v2/files/download?path= (streamed; Range supported)
     → octet-stream
```
- **Security:** a configured *shared-folder root*; every `path` is cleaned and
  MUST stay within root (reject `..` escapes). List/read outside root = 403.
  (Mirrors the desktop's existing `file_manager.rs` guarantees, server-side.)

**Logs** (per-client). Each client (desktop + each device) writes/reads its own
log bucket so support can see one client's activity.
```
GET /api/v2/logs/list                → { clients:[{client_id, size, updated}] }
GET /api/v2/logs/read?client_id=X&tail=N → { client_id, lines:[...] }
```
- Backed by `internal/logs` under `~/.winmux/server/logs/<client_id>.log`,
  size-capped + age-pruned (reuse Phase 75/75.1 janitor logic).

**Workspace** (cross-client shared state + events). The least-defined piece —
see Open Questions Q3.
```
GET  /api/v2/workspace/state             → current shared doc (JSON)
PUT  /api/v2/workspace/state             → replace/patch (optimistic version)
WS   /api/v2/workspace/events            → stream {type, payload, client_id, ts}
```
- v1 semantics proposal: a versioned JSON document + an append-only event
  stream; last-writer-wins with a `version` guard; no CRDT yet.

**Meta**
```
GET /healthz                 (unauthed) → { ok, version }
GET /api/version             → { name:"winmux-server", version, api_versions, min_client }
GET /api/openapi.json        → generated OpenAPI 3.1 spec
```

### 4.3 Auth & per-client scoping (`internal/auth`)
- **Desktop** presents the daemon bearer token (as today; read from the token
  file over the tunnel).
- **Mobile devices** present their **paired device token** (Phase 70). Each
  request is scoped to a `client_id` (device id, or `"desktop"`).
- Scopes gate subsystems: e.g. a device may be allowed `insights:read` +
  `chat:*` but not `hygiene:kill` or `files:*` unless granted at pairing time.
  (This is where the deferred Phase-74-QR "scopes checkboxes" finally land —
  server-enforced, not cosmetic.)

### 4.4 Streaming contract (WebSocket frames) — **pinned in S1**
> The mobile session flagged the real risk: **OpenAPI 3.1 describes REST, not
> WebSocket frames.** A generated REST SDK gives clients zero guarantees about
> the stream. So the WS frame schema is a **first-class, separately-versioned
> contract**, pinned now (S1) — not discovered later.

**Approach.** Every WS frame is a JSON object with a required discriminator
`"type"`; each `type` has a JSON-Schema. The set of schemas is published as an
**AsyncAPI 2.6** document (`asyncapi.json`, alongside `openapi.json`) and, for
clients that don't consume AsyncAPI, as a plain `frames.schema.json`
(`oneOf` keyed on `type`). SDK generation emits typed frame unions for both
Kotlin and TS from these schemas. CI drift-guards them like the REST spec.

**Frame envelope (all frames):**
```jsonc
{ "type": "<discriminator>", "ts": 1782900000, "session_id": "<uuid>",
  "seq": 42, /* monotonic per session, for gap detection + resume */ ...type-specific }
```

**Chat stream frames (existing behavior, now contract-pinned)** — the current
`chat_parser.go` already normalizes Claude stream-json into these; S1 freezes
their shapes as the v2 contract:

| `type` | when | key fields |
|---|---|---|
| `assistant_delta` | streamed assistant text | `text` |
| `tool_use` | Claude invokes a tool | `tool_name`, `tool_input`, `tool_id` |
| `tool_result` | tool returns | `tool_id`, `content`, `is_error` |
| `notification` | passive lifecycle hook | `subkind`, `title`, `summary` |
| `hook_request` | **8b** blocking permission ask | `req_id`, `subkind`, `tool_name`, `tool_input`, `decision_required` |
| `hook_resolved` | a client answered / timed out | `req_id`, `decision`, `reason` |
| `status` | session state change | `status` (active/waiting_input/waiting_hook/…) |
| `error` | stream/parse error | `message` |

**Client→server frames (commands):**
| `type` | effect |
|---|---|
| `user_input` | `{ content }` → echoed to all subscribers as a `user_input` event; fed to Claude stdin once the engine is attached |
| `hook_decision` | `{ req_id, decision: allow\|deny }` → resolves the pending hook, broadcast to all subscribers as `hook_resolved` |
| `interrupt` | `{}` → echoed to subscribers as an `interrupt` event |
| `unsubscribe` | `{}` → detaches this connection |

> **S4.3 — contract LOCKED (2026-07-02).** No client is locked, so this shape is
> now canonical (implemented in `internal/workspace/frames.go` as typed Go
> values; drift-guarded by `TestFrameWireShapes`). Decisions:
> - **Discriminator `type`** (not `kind`) — idiomatic for kotlinx
>   `@JsonClassDiscriminator`, TS tagged unions, AsyncAPI/JSON-Schema.
> - **Flat frames** — envelope (`seq`/`session_id`/`ts`) sits beside the
>   type-specific fields; the natural target for a kotlinx sealed base class + a
>   TS discriminated union. Not nested under a `data` key.
> - **snake_case everywhere** — matches the REST surface (`session_id`,
>   `req_id`, `frame_version`, `client_id`, `tool_name`, `is_error`,
>   `resolved_by`).
> - **Required fields explicit** — session events require
>   `type,seq,session_id,ts`; `hello` requires `type,frame_version,session_id,client_id`.
> - **Three families** — control (`hello`), server→client session events,
>   client→server commands.
> - The Phase-69 chat content frames (`assistant_text`, `tool_use`,
>   `tool_result`, `status`, `error`, `notification`) are carried in the schema
>   from day one but only emitted once a Claude session is attached (§16).
>
> Canonical machine schema: **`frames.schema.json`** (JSON-Schema 2020-12, the
> SDK source), published at `/api/frames.schema.json` alongside `asyncapi.json`
> (AsyncAPI 2.6, full per-type messages + a subscribe/publish `oneOf`). Both
> embedded + served with CORS.

**Multi-client attach (8a) + broadcast (8b) semantics:**
- A session has **N subscribers**. Every server-origin frame fans out to all
  subscribers (they see identical chat/tool progression).
- A `hook_request` is **pending** until the *first* `hook_decision` from *any*
  subscriber (or timeout); the resolution is broadcast to all as
  `hook_resolved` so late/other clients converge. (Maps directly onto the
  existing `pendingHooks[req_id] chan` in `chat_hookrpc.go`.)
- `seq` lets a reconnecting client detect gaps; **replay/resume** (buffer last
  K frames per session) is an S3 item, not S1.

**Versioning.** Frame contract version travels in the WS open response
(`{"type":"hello","frame_version":2}`); a client refusing an unknown
`frame_version` is a hard, visible failure — never silent drift.

---

## 5. Data & config (`internal/config`)
- Consolidate the currently-separate SQLite files (`metrics.db`, `chat.db`)
  under `~/.winmux/server/` with one opener + a migration runner. Keep them as
  separate DBs (different lifecycles/retention) but behind one config package.
- Device tokens, workspace state, per-client log index live here too.
- Startup runs forward-only migrations keyed by a `schema_version` table.

---

## 6. Client SDK generation (74.C)

> **S4.1 DONE (2026-07-02) — huma adopted, scoped to the client-SDK surface.**
> The client-facing HTTP surface — **version/health negotiation + Files (5) +
> Logs (3)** — is now **typed huma operations** (`internal/api/huma.go`,
> `internal/files/huma.go`, `internal/logs/huma.go`). `/api/openapi.json` is
> **generated from the handlers** (huma reflection, OpenAPI 3.1) — the
> hand-authored `openapi.json` is deleted. Contract preserved byte-for-byte
> (same params/status/headers/JSON); every pre-existing test passes unchanged.
> Emit the spec with `winmux-server openapi` (nil providers, no running server)
> — the SDK pipeline + CI drift-guard consume that.
>
> **Scope decision:** **Insights stays on raw stdlib handlers and is excluded
> from the generated SDK spec.** It is a desktop-internal Monitor API — dynamic
> `map[string]any` metric/docker/process payloads consumed by the desktop's
> **Rust** client (`insights_fetch` over SSH), never by a generated Kotlin/TS
> SDK. Freezing those loose maps into huma types would be churn with no SDK
> consumer. The SDK spec therefore describes exactly the client contract; the
> Insights API is documented separately (`docs/winmux-server/API.md`).
>
> **Binary/streaming/multipart** kept identical via huma's native mechanisms:
> Files read/download → `huma.StreamResponse` (raw octet-stream + custom
> headers), upload → `huma.MultipartFormFiles`, Logs stream → `huma/sse`
> (`event: line`, plus an `error` event for a bad id — SSE carries errors as
> events since the 200 stream is already open; the S2 pre-stream 400 is the only
> intentional wire delta, on an untested convenience endpoint). The `$schema`
> link transformer is disabled (`CreateHooks = nil`) so response bodies stay
> clean.
>
> **Compat shim:** each subsystem keeps `RegisterRoutes(mux, auth)` (builds a
> local huma API over the mux) so its own unit tests mount unchanged; production
> calls `RegisterHuma(sharedAPI)` on the one server-wide API that backs the spec.

**Recommended approach: OpenAPI-first via a typed Go framework.**
- Define handlers with typed input/output structs; emit **OpenAPI 3.1**
  automatically. Candidate libs (Open Question Q4):
  - **`huma`** (danielgtaylor/huma) — typed handlers, validation, OpenAPI 3.1
    out of the box, stdlib-compatible. *Recommended.*
  - `swaggo/swag` — annotation-based; more boilerplate, less type-safety.
- Generate SDKs from `openapi.json` with `openapi-generator-cli`:
  - Kotlin → `sdk/kotlin/` (retrofit/moshi or ktor client).
  - TypeScript → `sdk/typescript/` (fetch-based, tree-shakeable).
- **Build integration:** a `make sdk` / npm script regenerates both on server
  change; CI fails if `openapi.json` or the SDKs are stale (drift guard).
- **Version-lock:** SDK package version == server version; a client refuses a
  server whose `api_versions` doesn't include the SDK's.

> Reality check: the **desktop** today calls the server through Rust
> (`insights_fetch` over SSH), not the TS SDK directly. The generated TS SDK is
> primarily for a future web/mobile-web client and for **contract tests**; the
> desktop can keep its Rust calls but they get validated against the same
> OpenAPI in CI. Mobile (Kotlin) is the primary SDK consumer. (Open Q5.)

---

## 7. Migration path & backward compatibility (74.D)

**Binary rename with alias.** Installer lays down `winmux-server`; keeps a
`winmux-insights` **symlink → winmux-server** so anything referencing the old
name still resolves. `winmux-server --version` prints `2.x`; the old
`winmux-insights --version` path (symlink) prints the same.

**Systemd unit.** New `winmux-server.service`; installer `disable --now` the old
`winmux-insights.service`, removes its unit, installs + starts the new one
(reuse the Phase 72.1 `sg docker` launch logic).

**Data preserved.** `~/.winmux/insights/` → migrated/renamed to
`~/.winmux/server/` on first 2.x boot: move `metrics.db`, `chat.db`, `token`,
`paired_devices`. Idempotent; keeps the same device tokens so **already-paired
phones keep working**.

**Client version detection.** Desktop reads `GET /api/version`:
- `1.x` (or `/healthz` only) → speak the **legacy** paths (`/current`, …).
- `2.x` → speak `/api/v2/...`.
The desktop ships a tiny adapter that picks the path set by detected version.

**Compat window.** The 2.x server keeps **legacy route aliases** (`/current`
→ `/api/v2/insights/current`, etc.) for **≥ 3 minor versions** so an old desktop
against a new server still works during rollout. Aliases are thin redirects to
the v2 handlers, logged as deprecated.

**Version lockstep.** `INSIGHTS_VERSION` (winmux-addons) becomes
`SERVER_VERSION`, tracks the Go `Version` const (now `2.x`). The desktop's
update-available check compares remote `winmux-server --version` to this.

---

## 8. Client contracts (desktop + mobile)

- **Desktop (Rust):** unchanged transport (reverse SSH tunnel). Gains a
  version-adapter for path selection. Validated against OpenAPI in CI (contract
  test), not forced onto the generated TS SDK.
- **Mobile (Kotlin):** consumes the generated Kotlin SDK exclusively; talks to
  the server via nginx+TLS with its device token; scoped by pairing grants.
- **Contract stability guarantee:** within a major (`2.x`) no breaking changes
  to existing v2 endpoints — only additive. Breaking changes ⇒ `/api/v3` + a
  new major + a compat window.

---

## 9. Testing strategy (74.F)
- **Unit** per `internal/*` package (pure logic: markDuplicates-style, parsers,
  path-scoping, token scoping). Runs on the Windows dev box (CGO-free).
- **Integration**: spawn `winmux-server` on a random port with a temp data dir,
  hit real endpoints (auth, files traversal rejection, hygiene kill safety,
  chat happy-path with a fake `claude`).
- **Contract**: generate SDKs, point them at the spawned server, assert the
  generated client + server agree (this is the drift guard with teeth).
- **E2E**: real desktop build + a mock mobile client → server, over a local
  tunnel; the pairing → chat → insights round-trip.
- CI gates: `go test ./...`, `openapi.json` up-to-date, SDKs up-to-date,
  `cargo`/`tsc` for the desktop adapter.

---

## 10. Version cadence
- `winmux-server` **2.0.0** at the refactor cut; SemVer thereafter.
- **PATCH** = server-internal fix, no API change (desktop needn't update).
- **MINOR** = additive endpoints/fields; SDKs regenerate; clients optional-update.
- **MAJOR** = breaking API ⇒ new `/api/vN` + compat window.
- Server version and SDK versions are locked equal.

---

## 11. Phased implementation plan + estimates

> A "sprint-day" = one focused day of implementation on this repo. Ranges
> reflect the biggest unknowns (module-cycle surgery in S1, workspace semantics
> in S3, SDK toolchain in S4). Estimates assume no scope growth.

| Sprint | Scope | Deliverable | Est. |
|---|---|---|---|
| **S1** | Rename + module boundaries | `server/` monorepo dir, `core` interface layer breaking the WS↔session↔RPC cycle, all existing subsystems moved behind interfaces, **all current tests green**, binary+systemd rename with `winmux-insights` alias, legacy route aliases, version detection. No new features. | **3–4** |
| **S2** | Files API + Logs API | `/api/v2/files/*` (root-scoped, traversal-safe) + `/api/v2/logs/*` (per-client, janitor-bounded) + unit/integration tests. | **3** |
| **S3** | Workspace API + shared state | `/api/v2/workspace/state` + WS event bus + versioned last-writer-wins; needs Q3 answered first. | **4–5** |
| **S4** | SDK generation | huma/OpenAPI wiring, `openapi.json`, Kotlin + TS generators, build integration + CI drift guard, version-lock. | **3–4** |
| **S5** | Migration + polish | data move `insights/`→`server/`, 2.x auto-upgrade on install, desktop version-adapter, E2E + contract tests, docs (README/API/CLIENTS). | **3–4** |
| — | **Total** | | **~16–21 sprint-days** (≈ 3–4 focused weeks) |

Recommended ordering note: **S1 must fully land and ship (as 2.0.0 with feature
parity + compat aliases) before S2+**, so we de-risk the rename/migration
independently of the new features. Each later sprint ships as a 2.x MINOR.

---

## 12. Open questions

**Resolved** (see §0): Q0 (Phase 77), Q1 (`app/src-tauri/server/`), Q3 (workspace
= sessions+subscribers+pending, 8a+8b), Q4 (huma, deferred to S4), Q5 (server
UUID). **Still open** (non-blocking for S1):

- **Q2 — Files API scope.** Is the "shared folder" a single configured root, or
  should it expose the whole filesystem like the desktop file manager (with the
  same guards)? Single root is safer; full-FS is more powerful for mobile.
  *(S2 decision, not S1.)*
- **Q6 — Scopes model.** Confirm the per-device scope set (e.g. `insights:read`,
  `chat`, `files:read`, `files:write`, `hygiene:kill`, `workspace`). This is the
  home for the deferred QR "scopes checkboxes." *(auth lands the enum in S1; the
  concrete set can firm up through S2/S3.)*
- **Q7 — Hooks over HTTP?** The hook bridge is a separate localhost TCP RPC
  today. Fold it under `/api/v2/hooks/*` for uniformity, or leave it as-is
  (it's desktop↔server-local, never mobile)? Leaving it is less work.
- **Q8 — Compat window length.** "≥3 minor versions" of legacy aliases — good,
  or do you want a hard cutoff date?

---

## 13. Relationship to the parked work — RESOLVED
The `72-docker-group` stack **shipped as v0.4.2** (merged to `main`, tagged,
released, manifest updated). Phase 77 now proceeds on a **fresh `77-winmux-server`
branch off the updated `main`**, exactly as recommended. No unmerged-stack
merge risk.

## 14. Mobile contract dependency
`docs/PHASE-77-MOBILE-API-EXPECTATIONS.md` (owned by the mobile session) is the
authoritative list of what the Kotlin client needs. **Not present in the repo as
of S1 start.** Action: when it lands, reconcile it against §4 (REST) and §4.4
(WS frames) and log any deltas in DECISIONS. The §4.4 frame contract is written
to be the thing that doc pins against.

## 15. S1 module-boundary decisions (locked for implementation)
- **`internal/chat` owns the hook RPC *protocol*** (`HandleHookConn`) because it
  is inseparable from session state (per-session HMAC token, `pendingHooks`,
  `emit`). **`internal/hooks`** is reduced to the **thin TCP listener** that
  accepts connections and hands each to a `core.HookConnHandler` — this is the
  concrete break of the Phase-69 WS↔session↔RPC cycle: `hooks → core`,
  `chat → core`, `cmd` wires `hooks.Start(sessionManager)`.
- **`internal/core`** holds only interfaces + shared value types (no logic, no
  sibling imports): `HookConnHandler`, `AddrSink` (S1); `EventBus`,
  `SessionRouter` reserved for S3 workspace.
- **`files` / `logs` / `workspace`** are **compile-only stubs** in S1 (package +
  a `TODO(S2/S3)` doc) so the tree and `api` wiring exist without behavior.
- Metrics routes keep serving at their **legacy paths** (`/current`, …) AND new
  `/api/v2/insights/*`, sharing one handler, for the compat window.

### 15.1 S1.d migration decisions (locked)
- **Binary + symlink:** the installer uploads `winmux-server`, chmods it, and
  symlinks `winmux-insights → winmux-server` so any old reference resolves.
- **systemd:** installs `winmux-server.service`; disables + removes the old
  `winmux-insights.service`.
- **Data dir stays `~/.winmux/insights`** for S1 (the binary's default) so an
  in-place 1.x→2.x upgrade preserves the token + metrics.db + chat.db +
  paired_devices — **paired phones keep working**. The rename to
  `~/.winmux/server` is deferred to **S5** (data-move on first 2.x boot) to keep
  S1 low-risk. `INSIGHTS_VERSION = "2.0.0"` (lockstep with `core.Version`).
- **Detect** tries `winmux-server --version`, falling back to the
  `winmux-insights` name (symlink / pre-2.x install) during the upgrade window.
  **Uninstall** tears down both names (unit, binary, symlink).
- **Superseded, delete after review:** the old `app/src-tauri/insights/` Go
  source + `resources/winmux-insights-linux-*` binaries are no longer referenced
  by the desktop (addons.rs now embeds `winmux-server-linux-*`). Left in place
  on-branch so the S1 diff stays a pure add + a small addons.rs delta; removed as
  a cleanup once Yossi has reviewed the module split.

## 16. S3 — Workspace shared state (implemented)
State model (`internal/workspace`): **Workspace** (server UUID `ws_…`) → **Session**
(`sess_…`) → an append-only **Event** log (monotonic `seq` per session) + live
**subscribers** + **PendingRequest**s. All in `workspace.db` (SQLite,
`SetMaxOpenConns(1)` → race-free seq).

- **8a multi-attach:** `Subscribe(session, client, cursor)` replays every event
  after the cursor, then the client streams live frames (dedup by `seq`). Every
  `Publish` fans out to all subscriber channels. Proven: 2 WS clients on one
  session both receive a published event.
- **8b broadcast + answer:** a `hook_request` event reaches all subscribers; the
  FIRST `hook_decision` wins via an atomic `UPDATE … WHERE resolved_by=''`
  (idempotent per req_id); a `hook_resolved` event is broadcast with
  `resolved_by`. Proven: 10 concurrent deciders → exactly one winner; over WS,
  A decides and B sees the resolution.
- **Frames (§4.4):** flat JSON keyed on `type`; the WS `hello` advertises
  `frame_version`. Client→server: `user_input`, `hook_decision`, `interrupt`,
  `unsubscribe`.
- **FCM (§7, deferred):** `core.NotificationSender` with `NoopSender` shipped;
  real FCM (register token + push on hook timeout with a minimal payload) is a
  later sprint — the interface + the pending-request timeout field are in place.
- **S3.d — SCOPE REFINED (Yossi, 2026-07-02):** there are **no mobile devices in
  production**, so mobile-facing backward compat is not required.
  - The legacy Claude-chat HTTP surface (`/api/claude/session[s]`,
    `/api/claude/session/`, `/ws/claude/session/`) is **RETIRED → 410 Gone** — a
    clean break, not a migration. Clients drive Claude through
    `/api/v2/workspace/*` now; `workspace_id` is server-authoritative from day
    one (no old-UUID reconciliation).
  - **Pairing (`/api/pairing/*`) STAYS** — desktop-facing (Monitor QR + device
    tokens); Insights + Pairing keep full backward compat. Only the mobile-facing
    chat surface breaks. A `ws_default` workspace is still ensured at boot.
  - The chat **engine** (SessionManager + stream-json + hook RPC bridge) is kept
    as internal machinery. **Follow-up (when mobile consumes the new API):** wire
    Claude-spawn into a workspace `claude_chat` session so it runs `claude` and
    streams stdout into the workspace event log (engine↔substrate). Kotlin frame
    types are NOT locked, so that wiring uses the cleanest per-`type` schema.
