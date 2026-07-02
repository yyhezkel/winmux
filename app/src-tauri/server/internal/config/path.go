package config

// path.go — make the daemon resolve user-installed CLIs (notably `claude`) the
// way the user's terminal does. systemd starts services with a minimal PATH
// (often just /usr/bin:/bin), so a tool in ~/.local/bin, an npm global dir, or
// an nvm node dir is invisible — `exec.LookPath("claude")` then fails and a
// claude_chat session errors with "executable file not found in $PATH".

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// AugmentUserPath merges the user's login-shell PATH plus common CLI install
// dirs into the daemon's PATH. Call once at startup, before any subprocess
// spawn (and before the chat SessionManager resolves `claude`). Idempotent.
func AugmentUserPath() {
	var extra []string
	// A login+interactive shell sources the user's profile AND .bashrc → the PATH
	// they actually use (CLI installers add their dir in either). Interactive
	// warnings (no TTY) go to stderr; we only read stdout.
	if out, err := exec.Command("bash", "-lic", `printf %s "$PATH"`).Output(); err == nil {
		extra = append(extra, splitPath(strings.TrimSpace(string(out)))...)
	}
	if home, err := os.UserHomeDir(); err == nil {
		extra = append(extra,
			filepath.Join(home, ".local", "bin"),
			filepath.Join(home, "bin"),
			filepath.Join(home, ".npm-global", "bin"),
		)
	}
	extra = append(extra, "/usr/local/bin")
	if merged := MergePaths(os.Getenv("PATH"), extra); merged != "" {
		_ = os.Setenv("PATH", merged)
	}
}

// MergePaths returns base with any dirs from extra not already present appended
// (base entries kept first, order + dedupe preserved).
func MergePaths(base string, extra []string) string {
	sep := string(os.PathListSeparator)
	seen := map[string]bool{}
	var out []string
	add := func(d string) {
		if d != "" && !seen[d] {
			seen[d] = true
			out = append(out, d)
		}
	}
	for _, d := range splitPath(base) {
		add(d)
	}
	for _, d := range extra {
		add(d)
	}
	return strings.Join(out, sep)
}

func splitPath(p string) []string {
	if p == "" {
		return nil
	}
	return strings.Split(p, string(os.PathListSeparator))
}
