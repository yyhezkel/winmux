// Package config holds winmux-server's generic infrastructure shared by every
// subsystem: the API token, filesystem paths, and the log janitor (size-cap +
// age-prune, ported from Phase 75/75.1). Leaf-ish — depends only on the stdlib.
package config

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// RandHex returns n random bytes as a hex string (API + per-session tokens).
func RandHex(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		// crypto/rand should never fail; fall back to a fixed-length zero
		// string rather than panicking the daemon at startup.
		return hex.EncodeToString(make([]byte, n))
	}
	return hex.EncodeToString(b)
}

// LoadOrCreateToken reads the token file, creating a fresh 32-byte token on
// first run (mode 0600).
func LoadOrCreateToken(path string) string {
	if b, err := os.ReadFile(path); err == nil {
		if t := strings.TrimSpace(string(b)); t != "" {
			return t
		}
	}
	t := RandHex(32)
	_ = os.WriteFile(path, []byte(t+"\n"), 0o600)
	return t
}

// TailFile returns the last n lines of a file (size-capped logs, dependency-free).
func TailFile(path string, n int) []string {
	data, err := os.ReadFile(path)
	if err != nil {
		return []string{}
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) == 1 && lines[0] == "" {
		return []string{}
	}
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return lines
}

// ─── log hygiene (Phase 75/75.1) ─────────────────────────────────────────────

// RotateIfBig renames the log to .1 when it exceeds max bytes (used at boot,
// before the log fd is opened).
func RotateIfBig(path string, max int64) {
	if fi, err := os.Stat(path); err == nil && fi.Size() > max {
		_ = os.Rename(path, path+".1")
	}
}

// rotateCopyTruncate bounds a log WITHOUT breaking an open append fd: copy the
// current contents to <path>.1 then truncate the original in place.
func rotateCopyTruncate(path string, max int64) {
	fi, err := os.Stat(path)
	if err != nil || fi.Size() <= max {
		return
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return
	}
	if err := os.WriteFile(path+".1", data, 0o644); err != nil {
		return
	}
	_ = os.Truncate(path, 0)
}

// pruneIfOld deletes a log file untouched for longer than maxAge.
func pruneIfOld(path string, maxAge time.Duration) {
	if fi, err := os.Stat(path); err == nil && time.Since(fi.ModTime()) > maxAge {
		_ = os.Remove(path)
	}
}

// LogJanitor keeps every winmux server-side log bounded (size-cap via
// copy-truncate + age-prune). Runs at boot then every 30 min.
func LogJanitor(insightsLog, home string, stop <-chan struct{}) {
	const sizeCap = 1 << 20
	const maxAge = 7 * 24 * time.Hour
	hookLog := filepath.Join(home, ".winmux", "hook-debug.log")
	installLog := filepath.Join(home, ".winmux", "logs", "mobile-install.log")
	sweep := func() {
		rotateCopyTruncate(insightsLog, sizeCap)
		rotateCopyTruncate(hookLog, sizeCap)
		rotateCopyTruncate(installLog, 512<<10)
		for _, p := range []string{
			insightsLog + ".1", hookLog, hookLog + ".1", installLog, installLog + ".1",
		} {
			pruneIfOld(p, maxAge)
		}
	}
	sweep()
	t := time.NewTicker(30 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
			sweep()
		}
	}
}
