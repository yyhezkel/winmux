# Installing AI coding agents

winmux integrates with three first-class AI coding agents. They're all
optional — pick whichever ones you want. The **Provisioning Wizard**
(sidebar → ☁ Provision server) can install any subset of them on a
fresh remote Linux box, but you can also install them manually anywhere
using the commands below.

| Agent | Vendor | Install medium | License model |
|---|---|---|---|
| [Claude Code](https://docs.claude.com/en/docs/claude-code) | Anthropic | `curl` installer + winget + npm | Subscription / API key |
| [Codex CLI](https://github.com/openai/codex) | OpenAI | npm | Plus / Pro plan or API key |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | Google | npm | Free tier + paid plans |

## Claude Code

### Remote Linux (what the Provisioning Wizard runs)

```bash
curl -fsSL https://claude.ai/install.sh | bash
```

Self-contained installer — no npm prerequisite. Drops a static binary
launcher and updates your shell's PATH on next login.

### Windows desktop

Three options:

```pwsh
# Option 1 — winget (recommended)
winget install Anthropic.ClaudeCode

# Option 2 — PowerShell installer
irm https://claude.ai/install.ps1 | iex

# Option 3 — manual download
# https://docs.claude.com/en/docs/claude-code → Install
```

### macOS

```bash
curl -fsSL https://claude.ai/install.sh | bash
```

(Same installer as Linux.)

### Authentication

After install, run `claude` in any terminal. On a desktop OS this opens
a browser to console.anthropic.com. On a headless server (or when the
browser auto-open fails), `claude` prints a code + URL that you open
on a phone or another machine. The credentials persist in
`~/.claude/credentials.json`.

Alternative for CI / headless: set `ANTHROPIC_API_KEY=sk-ant-...`. Get
a key from [console.anthropic.com/api-keys](https://console.anthropic.com/settings/keys).

## Codex CLI

### Install (any platform with Node.js)

```bash
npm install -g @openai/codex
```

Codex doesn't ship a self-contained installer — Node.js must already be
on PATH. The Provisioning Wizard's "Install Node.js LTS" step covers
this; for manual installs use [nvm](https://github.com/nvm-sh/nvm),
[fnm](https://github.com/Schniz/fnm), or the official Node.js installer.

### Authentication

Two paths:

```bash
# Desktop (has a browser)
codex login

# Headless (server / SSH session)
codex login --device-auth
```

`--device-auth` prints a short code; open the printed URL on any
device, paste the code, and the server picks up the token. Credentials
persist in `~/.codex/`.

Alternative for CI: set `OPENAI_API_KEY=sk-...`. Get a key from
[platform.openai.com/api-keys](https://platform.openai.com/api-keys).
(Note: Codex's authenticated mode bills against your ChatGPT Plus/Pro
subscription; the API-key path bills against your OpenAI API account.)

## Gemini CLI

### Install (any platform with Node.js)

```bash
npm install -g @google/gemini-cli@latest
```

Same Node.js prerequisite as Codex.

### Authentication

Two paths:

```bash
# Free tier — sign in with Google
gemini

# Headless or API-key mode
export GEMINI_API_KEY=AIza...
gemini
```

`gemini` (no args) walks you through OAuth — opens a browser on
desktop, falls back to a device-code flow on headless servers.
Credentials persist in `~/.gemini/`.

For API-key mode get one from [aistudio.google.com/apikey](https://aistudio.google.com/apikey).

## Using the Provisioning Wizard

The wizard's **Configure** step lists every agent above as a checkbox
under "AI coding agents". Three things to know:

1. **Pick any subset.** The `default` profile checks only Claude Code;
   `all-agents` enables all three.
2. **Codex and Gemini both need Node.js.** Make sure
   "Install Node.js LTS" is also checked if you want either of them.
   The step will fail fast with a clear error if Node isn't on PATH —
   nothing destructive.
3. **Authentication is manual.** None of the wizard steps can sign you
   in (that needs a token only you should hold). The execute-step log
   prints the exact `<agent> login` command to run on the remote after
   the wizard finishes.

## Combining agents

All three agents can coexist on the same machine without conflict —
they each install into distinct locations (`~/.claude/`, `~/.codex/`,
`~/.gemini/`), expose distinct CLI binaries (`claude`, `codex`,
`gemini`), and have separate auth state. You can use one in one
winmux pane and another in a sibling pane.

The **agent permission hook** integration (Settings → Hooks) currently
only ships for Claude Code — Codex and Gemini don't yet have a public
hook contract analogous to Claude's `~/.claude/settings.json` hooks
array. As Codex / Gemini add hook APIs, winmux's `setup-hooks --agent
<name>` will grow corresponding adapters.

## Updating

| Agent | Update command |
|---|---|
| Claude Code | `claude update` (in-place self-update) |
| Codex CLI | `npm update -g @openai/codex` |
| Gemini CLI | `npm update -g @google/gemini-cli` |

## Uninstalling

| Agent | Uninstall |
|---|---|
| Claude Code | `claude uninstall` or remove `~/.claude/` + the PATH bumps |
| Codex CLI | `npm uninstall -g @openai/codex` + remove `~/.codex/` |
| Gemini CLI | `npm uninstall -g @google/gemini-cli` + remove `~/.gemini/` |
