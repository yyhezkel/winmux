# CLAUDE.md

This file is read at the start of every Claude session working on winmux. Keep it small. Deep references live in `docs/`.

## Where to start

- `docs/ARCHITECTURE.md` — system map
- `docs/CONTRIBUTING.md` — recipes, style, commit conventions
- `docs/RELEASING.md` — version cut process
- `docs/DECISIONS.md` — **READ FIRST**: open threads + decisions log
- `docs/COMPETITIVE-SCAN.md` — survey of 8 winmux projects, ideas inventory, Secrets Vault design
- `docs/IDEAS-RANKING.md` — decision table for the ideas inventory (MUST / SHOULD / COULD)

## Decisions & open threads

When an idea or design question comes up:

1. If it's resolved in the same message, do it — no log entry needed.
2. If a decision is made but action is deferred, log it under **Decided** in `docs/DECISIONS.md` with the outcome and a deferral note.
3. If it stays open (user hasn't decided, blocked on input, flagged for later), log it under **Open** in `docs/DECISIONS.md` with options and current state.

When starting a new session, scan the **Open** section. Don't let threads die silently — if something's been pending a while, surface it.

## Off-limits paths

- `backup-phase23-*` folders — never touch
- Repo-root `.bat` / `.ps1` helper scripts the user maintains — never touch
- `release_notes.md` — do not commit
- `remote-manifest.json` timestamp churn — discard unless the SHA actually changed
- Linux CLI binary rebakes itself on release builds (CARGO_PKG_VERSION) — expected, commit as part of the release

## Release safety

- Never push a half-done release. If a step fails for a real reason, stop and report.
- `app.exe` running on the user's machine causes `os error 32` during NSIS bundler cleanup — cosmetic; the binary + bundles produced fine.
- v0.2.3+: updater uses native `ureq` + `rustls` (no more PowerShell).

## Communication

- User: Yossi (`yyhezkel@gmail.com`). Prefers Hebrew, terse, action-oriented replies.
- Phase numbering: stable in commit history. Sub-numbers (`23.J`) for follow-ups. No reuse.
- Commit format per `docs/CONTRIBUTING.md`.

## Absolute Rules — Do Not Violate

1. **Never log PTY input or output content.** Only metadata (pane ID, byte counts, error kinds). User shell content is private.
2. **Never store SSH passphrases or sudo passwords in plaintext at rest.** Use DPAPI (`CryptProtectData`) when persistence is necessary; otherwise keep in memory only.
3. **Never build shell commands by string concatenation.** Use `Command::new(...).arg(...)` arrays. The agent and provisioning paths are the only places this is enforced repeatedly — don't drift from it.
4. **No `unwrap()` or `expect()` in non-test Rust** outside the `main()` boot path. Use `?` or `.map_err(...)` and surface a clean error.
5. **No `any` in TypeScript.** Use `unknown` and narrow, or define a type. Tauri command return types are always explicit.
6. **All Tauri commands return `Result<_, String>`.** The frontend handles the error; don't `panic!`.
7. **Workspace persistence is atomic.** Write to `<file>.tmp` then `rename` to the target. Never partial writes to `workspaces.json` / `settings.json`.
8. **Never expose the tunnel HMAC token to logs.** Treat it like a password.
9. **`dlog()` is user-visible** (lands in `%APPDATA%\winmux\debug.log`). `tracing::*` is engineer-only (only emitted in dev builds). Pick the right one based on audience.
10. **Don't push a half-done release.** If any step in RELEASING.md fails for a real reason (not the `os error 32` NSIS cleanup false-alarm), stop and report.
11. **Don't touch `backup-phase23-*/` or repo-root `.bat` / `.ps1` helper scripts.** Don't commit `release_notes.md`.
12. **`remote-manifest.json` timestamp churn is cosmetic.** Discard unless the embedded SHA actually changed.
