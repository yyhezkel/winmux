# Releasing winmux

Cutting a new version is a six-step manual checklist for now. CI is on
the roadmap; until then this is your runbook.

## 1. Bump the version

Update `version` in:

- `app/src-tauri/Cargo.toml` (workspace `[package]`)
- `app/src-tauri/cli/Cargo.toml`
- `app/src-tauri/mcp/Cargo.toml`
- `app/src-tauri/tauri.conf.json` (`"version"` field)
- `app/package.json`

Commit as `chore: bump to vX.Y.Z`.

## 2. Build the release

```pwsh
cd app
powershell -ExecutionPolicy Bypass -File ./scripts/build-release.ps1
```

This wrapper sets `RUSTFLAGS=--remap-path-prefix=...` so embedded
panic-location strings don't carry the build machine's `$HOME`. The
output is:

- `app/src-tauri/target/release/app.exe`
- `app/src-tauri/target/release/bundle/msi/winmux_X.Y.Z_x64_en-US.msi`
- `app/src-tauri/target/release/bundle/nsis/winmux_X.Y.Z_x64-setup.exe`

Verify the scrub:

```pwsh
grep -aoc $env:USERNAME app/src-tauri/target/release/app.exe
# should be 0
```

## 3. Tag

```pwsh
git tag -a vX.Y.Z -m "winmux vX.Y.Z — <one-line summary>"
git push origin vX.Y.Z
```

## 4. Publish the GitHub Release

```pwsh
gh release create vX.Y.Z `
  --title "winmux vX.Y.Z" `
  --notes-file release_notes.md `
  app/src-tauri/target/release/bundle/msi/winmux_X.Y.Z_x64_en-US.msi `
  app/src-tauri/target/release/bundle/nsis/winmux_X.Y.Z_x64-setup.exe
```

## 4½. Bump hook specs (only when hooks changed)

If this release changes any of `hooks/*.json` (added a Claude Code
event, switched a matcher, renamed a subcommand…):

1. Bump `winmux_hooks_version` in the affected `hooks/<agent>.json`
   file (semver: bump major if events were removed/renamed, minor for
   additive changes, patch for matcher tweaks).
2. Bump the matching `BUNDLED_CLAUDE_VERSION` constant in
   `app/src-tauri/cli/src/hooks.rs` so the bundled fallback stays in
   sync (and matches what `setup-hooks --source bundled` writes).
3. In `manifest.json`, bump the matching `hooks.<agent>.version`
   field so the desktop's outdated-check picks up the new version
   on the next SSH connect.

The desktop's `check_remote_hooks` (in `updater.rs`) compares each
remote's `~/.claude/settings.json::winmux_meta.hooks_version` against
manifest's `hooks.claude-code.version`. When a server is on an older
version AND the user hasn't dismissed that version (Settings → Claude
→ Hook updates), a banner fires.

## 5. Update `manifest.json`

The updater (`updater.rs`) polls
`https://raw.githubusercontent.com/yyhezkel/winmux/main/manifest.json`
on startup and surfaces a banner when a newer version is available.
**This file must be updated for every release** — otherwise existing
installs won't know there's an update.

Workflow:

1. Get the SHA256s and sizes of the assets you just uploaded:

   ```pwsh
   gh release view vX.Y.Z --json assets
   ```

   Look for the `digest` (format `sha256:abcdef…`) and `size` fields.

2. Edit `manifest.json` at the repo root:

   ```json
   {
     "version": "X.Y.Z",
     "released_at": "<ISO8601 UTC timestamp>",
     "notes_url": "https://github.com/yyhezkel/winmux/releases/tag/vX.Y.Z",
     "msi_url": "https://github.com/yyhezkel/winmux/releases/download/vX.Y.Z/winmux_X.Y.Z_x64_en-US.msi",
     "msi_sha256": "<from gh release view>",
     "msi_size": <bytes>,
     "nsis_url": "https://github.com/yyhezkel/winmux/releases/download/vX.Y.Z/winmux_X.Y.Z_x64-setup.exe",
     "nsis_sha256": "<from gh release view>",
     "nsis_size": <bytes>,
     "min_supported_version": "<oldest version that should be told to upgrade>"
   }
   ```

3. Commit + push to `main`. `raw.githubusercontent.com` picks up changes
   within ~1 minute.

## 6. Verify the update banner

On a previous-version install:

1. Wait for the 3-second startup grace period; the updater task fires
   after that.
2. Look for the floating banner at the bottom centre: `winmux X.Y.Z is
   available — current X.Y.(Z-1)`. "Release notes" link should open
   the new tag's page.
3. Alternatively: Settings → Updates → "Check now" force-runs the
   poll without waiting.

If the banner doesn't appear:

- `winmux dev check-updates --pretty` from a terminal shows the parsed
  manifest + the version comparison result + the last-check ISO.
- Check `%APPDATA%\winmux\debug.log` for any `updater: fetch … failed`
  lines — typically DNS, certificate, or proxy issues.

## Caveats

- **Code-signing**: the MSI / NSIS bundles are not signed yet.
  SmartScreen will warn on first launch. Adding signing to the release
  flow is a future task — when it lands, the manifest schema may grow
  a `signature` field.
- **Auto-install**: only the *notification* part of update flow is
  implemented. Users still download the MSI manually. Real
  auto-install would need signing keys + a verified-download path.
- **Old versions**: bumping `min_supported_version` doesn't *force*
  an upgrade — it's just a hint the future updater can use to refuse
  to load workspace files written by versions newer than itself.
