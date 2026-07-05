package config

// path.go — make the daemon resolve user-installed CLIs (notably `claude`) the
// way the user's terminal does. systemd starts services with a minimal PATH
// (often just /usr/bin:/bin), so a tool in ~/.local/bin, an npm global dir, or
// an nvm node dir is invisible — `exec.LookPath("claude")` then fails and a
// claude_chat session errors with "executable file not found in $PATH".

import (
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strings"
)

// resolveHome returns the running user's home dir, robustly. systemd may start
// the daemon with $HOME UNSET — in which case os.UserHomeDir() errors and the
// per-home install dirs (~/.local/bin, where `claude` lives) never get added.
// user.Current() reads /etc/passwd, so it works regardless of $HOME.
func resolveHome() string {
	if h := strings.TrimSpace(os.Getenv("HOME")); h != "" {
		return h
	}
	if h, err := os.UserHomeDir(); err == nil && strings.TrimSpace(h) != "" {
		return h
	}
	if u, err := user.Current(); err == nil {
		return u.HomeDir
	}
	return ""
}

// nvmBinDirs returns the bin dirs of every installed nvm node version
// (~/.nvm/versions/node/<ver>/bin) — a common `claude` (npm-global) location
// that a non-TTY shell PATH won't surface.
func nvmBinDirs(home string) []string {
	entries, err := os.ReadDir(filepath.Join(home, ".nvm", "versions", "node"))
	if err != nil {
		return nil
	}
	var out []string
	for _, e := range entries {
		if e.IsDir() {
			out = append(out, filepath.Join(home, ".nvm", "versions", "node", e.Name(), "bin"))
		}
	}
	return out
}

// AugmentUserPath merges the user's login-shell PATH plus common CLI install
// dirs into the daemon's PATH. Call once at startup, before any subprocess
// spawn (and before the chat SessionManager resolves `claude`). Idempotent.
//
// Belt-and-suspenders: the per-home dirs are added from a ROBUSTLY-resolved
// home (not just os.UserHomeDir, which needs $HOME) and do NOT depend on the
// `bash -lic` probe succeeding — many distros strip login mode under systemd.
func AugmentUserPath() {
	var extra []string
	// Best-effort: the login+interactive shell PATH (CLI installers add their
	// dir in the profile/.bashrc). Failure here is fine — the explicit dirs
	// below are the load-bearing part.
	if out, err := exec.Command("bash", "-lic", `printf %s "$PATH"`).Output(); err == nil {
		extra = append(extra, splitPath(strings.TrimSpace(string(out)))...)
	}
	if home := resolveHome(); home != "" {
		extra = append(extra,
			filepath.Join(home, ".local", "bin"),
			filepath.Join(home, "bin"),
			filepath.Join(home, ".npm-global", "bin"),
			filepath.Join(home, "node_modules", ".bin"),
		)
		extra = append(extra, nvmBinDirs(home)...)
	}
	extra = append(extra, "/usr/local/bin", "/usr/bin", "/bin")
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
