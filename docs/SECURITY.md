# Security Threat Model

This document is a thinking aid, not a certification. It says what we're trying to protect, who we're trying to protect it from, what we're doing about it, and what's out of scope.

## Assets

1. **Workspace state** — `workspaces.json` in `%APPDATA%\winmux`. Names, hosts, ports, paths. Not a secret per se but reveals where the user works.
2. **SSH credentials** — key paths (the keys themselves live wherever the user put them), optional remembered passphrases (DPAPI-encrypted at rest, per D-006), HMAC tokens for reverse tunnels (in-memory only).
3. **Agent tokens** — Claude Code session state lives under `~/.claude` and `~/.codex` on the *remote* boxes; winmux itself doesn't store provider API keys.
4. **Audit log** — `debug.log` in `%APPDATA%\winmux`. PTY content is NEVER written there (CLAUDE.md Rule 1); only event metadata.
5. **Tunneled traffic** — local 127.0.0.1 listeners that bridge to remote ports via SSH. Plaintext on the wire (between localhost and our app), encrypted on the SSH side.

## Adversaries

- **A1 — Same-user processes.** Anything else running as the same Windows user. Treated as TRUSTED at the named-pipe layer (the pipe ACL admits same-user, per D-004). We do not try to defend against malware running as the user.
- **A2 — Malicious npm postinstall the agent ran.** Tool-use scripts execute via Claude Code on the remote. They run as the remote user. Risk: a postinstall script tries to talk back to our reverse-tunnel RPC.
- **A3 — Prompt injection in a document.** A README, a PDF, or output a tool produces contains text that instructs the agent to do something off-script. The agent itself is in scope of its own provider's safety; what winmux can prevent is the agent escalating into actions the user didn't approve.
- **A4 — Remote root.** A compromised remote box. Treated as ADVERSARIAL: the local SSH client must not implicitly trust anything the remote sends.
- **A5 — Network MITM** between local and remote. Standard SSH host-key trust model applies.

Explicitly **out of scope**: local Administrator, SYSTEM, kernel-mode adversaries, physical access to an unlocked machine, side-channel attacks. If the attacker is admin on the local box, winmux is not the layer that helps.

## Mitigations by adversary

### A1 (same-user processes)

- Named pipe ACL restricts to the current Windows user (default Tauri behavior + the pipe is created without explicit broader DACL). A second non-user process trying to connect gets ACCESS_DENIED before any winmux code runs.
- We DO authenticate over the pipe too (HMAC challenge/response from the reverse tunnel; the bundled CLI dials in with a token only winmux itself knows for that session). This isn't defense against A1 specifically — it's defense against a remote process that's been given a pipe handle by accident.

### A2 (malicious postinstall on the remote)

- Reverse-tunnel auth: only callers that know `WINMUX_TUNNEL_TOKEN` (a per-session 32-char random) can complete the RPC handshake. The token lives only in the SSH session's env, in the env file `~/.winmux/run/last.env` (mode 0600), and in our process memory.
- Bridge handshake (`tunnel.rs::perform_handshake`) rejects clients with the wrong token before any other byte is read.
- Forward auto-fetching is OFF by default and only fires for ports the WATCHER reported as LISTENING (Phase 39+). winmux's own reverse-tunnel port is explicitly filtered (Phase 39 C2 — also defended in the CLI via `WINMUX_SOCKET_ADDR` self-exclude).
- Forwards are LOCAL-only (`127.0.0.1`, not `0.0.0.0`); a forwarded port is not exposed to the LAN.

### A3 (prompt injection)

- Hooks system intercepts Claude Code permission requests on the remote and routes them through winmux (the OS-level toast / Feed item asks the *user*, not the model, for the decision). Default policy preset prompts on risky tools; the relaxed and auto presets are opt-in.
- The agent runs in its own process tree on the remote; it cannot inject into winmux's local UI by emitting text — at most it can persuade the user via what shows up in their terminal pane, which is the same risk as reading any text from a remote shell.

### A4 (compromised remote)

- Host key check (russh) trips on first connect and on key change. The `UNKNOWN_HOST` and `HOST_KEY_MISMATCH` errors surface in the UI and require explicit user confirmation. Known hosts stored at `%APPDATA%\winmux\known_hosts`.
- We do NOT auto-accept unknown host keys in any background path (the headless Phase 41 connect passes `accept_unknown_host: false`).
- The remote CLI binary is upload-verified by SHA-256 after each push (Phase 6.2). A compromised remote can't silently swap it because the bootstrap re-checks the hash on every connect.
- Tunneled traffic from the remote to localhost is bridged to the named pipe whose ACL still gates same-user. The HMAC handshake on the bridge prevents a non-tokened remote process from poking the RPC.

### A5 (network MITM)

- Standard SSH: encryption + host-key TOFU. We don't terminate TLS or add a second crypto layer.

## What we deliberately don't do

- We do not sandbox tool execution on the remote — the agent runs as the user, full PATH, full FS access. Sandboxing would have to be at the remote OS / container layer; winmux is the wrong place.
- We do not vet the contents of `workspaces.json` if the user edits it by hand. Path validation happens at use time, not at load time.
- We do not encrypt the audit log. Per CLAUDE.md Rule 1, the audit log doesn't contain PTY content; it does contain hostnames, user names, pane counts, error messages. If that's sensitive in the user's threat model, the file lives in `%APPDATA%` which already has Windows ACL protection against other users.

## Reporting a vulnerability

Email **yyhezkel@gmail.com** with a description and (if possible) a minimal repro. Do not open a public issue for unpatched vulnerabilities — coordinate disclosure timing first.

## Open security questions

- The bundled MCP exe (`winmux-mcp.exe`) connects to the same named pipe with the same auth surface as the user CLI. A future hardening: make MCP authenticate with a separate per-process token rather than the shared one.
- The Phase 38 keepalive timing (30s) is a tradeoff between detecting dead remotes vs background traffic the user might want to avoid in metered environments. Not configurable today.
- DPAPI binds to the Windows user account. If we ever support roaming user profiles or cloud-sync of `%APPDATA%`, secrets become unreadable on the second machine — not a vulnerability, just a UX limitation to document.
