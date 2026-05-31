# Architectural Decision Records

Lightweight ADRs — one decision per entry, why this and not the alternative, what we trade off. New decisions append at the bottom. Don't rewrite history; if a decision is superseded, add a new entry that says so.

Format per entry:

- **D-N — Short title**
- **Context:** what forced the choice
- **Decision:** what we did
- **Alternatives:** what we didn't do, and why not
- **Consequences:** good and bad

---

## D-001 — Tauri over Electron

- **Context:** Need a cross-architecture native desktop wrapper around a web UI. Project is Windows-first but the architecture shouldn't pin us to Win32 forever.
- **Decision:** Tauri 2 (Rust core + system WebView).
- **Alternatives:**
  - Electron: bundled Chromium adds 100+ MB to every install, every release. Auto-updater complexity. Memory baseline ~150 MB before doing anything.
  - Plain Win32 + WebView2: no good Rust wrapper, gives up the cross-arch path, ipc with the WebView is hand-rolled.
- **Consequences:** Single bundle is ~6 MB (vs ~100 MB Electron). System WebView means WebView2 version drift on user machines — partly absorbed by Tauri's check on first run. Rust ↔ JS bridge is via `invoke` / `emit` — sufficient for our needs.

## D-002 — SolidJS over React

- **Context:** Reactive frontend for a high-event-rate app (PTY streams, port events, RPC frames). React's render model would force `useMemo` / `useCallback` everywhere to avoid re-rendering a 50-tab terminal grid on every tick.
- **Decision:** SolidJS — fine-grained reactivity, no virtual DOM, no re-runs of components.
- **Alternatives:**
  - React + Zustand: well-trodden, but `useEffect` semantics fight us when state updates 10×/second.
  - Svelte: compiler-based, similar reactivity story, but smaller ecosystem and our team has no Svelte experience.
- **Consequences:** Smaller bundle, predictable updates, fewer footguns at high event rates. Smaller ecosystem — sometimes we write a tiny component instead of pulling a library.

## D-003 — russh over libssh2/libssh-rs

- **Context:** SSH client for the workspace connections, plus reverse tunnel + sftp.
- **Decision:** `russh` (pure-Rust async SSH client).
- **Alternatives:**
  - `ssh2`/`libssh2-sys`: C library bindings; sync I/O wrapped in `tokio::spawn_blocking`; build-time C dependency.
  - `libssh-rs`: similar — C dependency, harder cross-build story (especially for the bundled musl Linux CLI).
- **Consequences:** No C deps → reproducible cross-build to `x86_64-unknown-linux-musl` is straightforward. Pure-async fits the rest of the codebase. russh's API has had churn between versions — we've eaten one minor migration. The `russh_sftp` companion crate is less mature than libssh's sftp — works but we've debugged ETXTBSY on overwrite ourselves (Phase 39.D).

## D-004 — Windows named pipe over localhost TCP for RPC

- **Context:** The app's RPC server has to serve a Windows CLI, a bundled MCP exe, and reverse-tunneled clients from remotes. Needs auth + access control.
- **Decision:** Named pipe `\\.\pipe\winmux-<user>` with HMAC-SHA256 challenge/response.
- **Alternatives:**
  - 127.0.0.1 TCP on a fixed port: any process on the box (or any browser tab via DNS rebinding) can reach it; would need careful auth-on-every-request.
  - Unix socket: not portable to Windows.
- **Consequences:** Pipe ACLs default-restrict to the same user account — same-user processes are still trusted (acceptable per threat model). No port collision. Required us to build the listener pool (Phase 44) to handle concurrent connects without `ERROR_PIPE_NOT_AVAILABLE`. The reverse-tunnel bridge (tunnel.rs) converts the SSH-channel side into a pipe client on demand.

## D-005 — `ureq` + `rustls` over `reqwest` for the updater

- **Context:** Updater needs to fetch a small JSON manifest and download an installer ~10 MB.
- **Decision:** `ureq` (sync) + `rustls`. Was: a PowerShell helper script (v0.2.2 and earlier).
- **Alternatives:**
  - `reqwest` (async + tokio): pulls in tokio + hyper + a thousand transitive deps, doubles the binary size.
  - Continue with PowerShell: invisible to the type system, fails opaquely on machines with execution-policy lockdowns or no PowerShell at all.
- **Consequences:** Updater is now self-contained, no spawning of `powershell.exe`, no policy-related failures. `ureq`'s sync API is a bit awkward when called from tokio code — we run it in `spawn_blocking`. `rustls` means no OpenSSL.

## D-006 — DPAPI for secret persistence

- **Context:** Some persisted state is sensitive (SSH passphrases that the user chose to remember, future credentials). Can't store in plaintext; we don't want to bring in a full keyring abstraction.
- **Decision:** Windows DPAPI (`CryptProtectData` / `CryptUnprotectData`) for at-rest secrets. Per CLAUDE.md Rule 2.
- **Alternatives:**
  - Plaintext JSON: no.
  - Windows Credential Manager via `wincred`: per-credential GUI surface, painful for bulk reads, no clean way to enumerate ours.
  - Cross-platform `keyring` crate: another C dep on macOS/Linux (we're Win-only today; revisit when porting).
- **Consequences:** Secrets are bound to the current Windows user account — copying `%APPDATA%\winmux` to another machine fails decryption. That's the correct behavior. The user can't manually back up secrets (unintentional but acceptable).

## D-007 — Per-workspace identity inherits to panes

- **Context:** Workspaces have a name, color, emoji. Panes inside also have those fields. Without an inheritance rule, every pane is unnamed by default.
- **Decision:** Effective identity = pane's own field if set, else workspace's. Set in `effectiveIdentity()` in `app/src/types.ts`. Persisted state stores only non-default values; the effective value is computed at display time.
- **Alternatives:**
  - Pane copies workspace identity at create time: changing the workspace later doesn't propagate to existing panes, surprising.
  - Pane must always set its own: noisy UX, every pane needs naming.
- **Consequences:** Cheap rename: rename the workspace, all auto-named panes update. Panes can still override locally (e.g., "trying to find the X bug" annotation per pane). The display layer has to remember to call `effectiveIdentity` and not just read pane fields.
