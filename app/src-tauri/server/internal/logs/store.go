// Package logs serves per-client + server log storage (/api/v2/logs/*,
// PHASE-77-DESIGN §4.2). Layout under <dataDir>/logs:
//
//	logs/clients/{client_id}/{file}.log   — per paired client (0700, daemon-only)
//
// plus a "server" pseudo-client that surfaces the daemon's own log. Names are
// strictly validated so a client_id / file can never traverse out of the tree.
package logs

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"winmux-server/internal/config"
)

// ErrBadID is returned for an invalid client id or file name.
var ErrBadID = errors.New("invalid client id or file name")

// Store manages the on-disk log tree.
type Store struct {
	clientDir string // <dataDir>/logs/clients
	serverLog string // the daemon's own log (surfaced as client "server")
}

// NewStore roots the tree at <dataDir>/logs and remembers the server log path.
func NewStore(dataDir, serverLog string) (*Store, error) {
	cdir := filepath.Join(dataDir, "logs", "clients")
	if err := os.MkdirAll(cdir, 0o700); err != nil { // 0700 — readable only by the daemon user
		return nil, err
	}
	return &Store{clientDir: cdir, serverLog: serverLog}, nil
}

// safeName rejects anything that could escape the tree (no separators, no "..").
func safeName(s string) bool {
	if s == "" || len(s) > 128 || s == "." || s == ".." {
		return false
	}
	for _, r := range s {
		ok := (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') ||
			(r >= '0' && r <= '9') || r == '_' || r == '-' || r == '.'
		if !ok {
			return false
		}
	}
	return true
}

// Append writes "unixts line\n" to clients/{id}/{file}. Best-effort; invalid
// names are dropped rather than erroring the caller.
func (s *Store) Append(clientID, file, line string) {
	if !safeName(clientID) || !safeName(file) {
		return
	}
	dir := filepath.Join(s.clientDir, clientID)
	if os.MkdirAll(dir, 0o700) != nil {
		return
	}
	f, err := os.OpenFile(filepath.Join(dir, file), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "%d %s\n", time.Now().Unix(), line)
}

// ClientInfo is one entry in the /logs/list response.
type ClientInfo struct {
	ClientID    string   `json:"client_id"`
	DeviceName  string   `json:"device_name"` // filled once per-client auth scoping lands
	LogDir      string   `json:"log_dir"`
	Files       []string `json:"files"`
	LatestEntry int64    `json:"latest_entry"` // unix mtime of the newest file
}

// ListClients returns the server pseudo-client + every per-client dir.
func (s *Store) ListClients() []ClientInfo {
	out := []ClientInfo{}
	if s.serverLog != "" {
		ci := ClientInfo{ClientID: "server", LogDir: filepath.Dir(s.serverLog), Files: []string{filepath.Base(s.serverLog)}}
		if fi, err := os.Stat(s.serverLog); err == nil {
			ci.LatestEntry = fi.ModTime().Unix()
		}
		out = append(out, ci)
	}
	des, _ := os.ReadDir(s.clientDir)
	for _, de := range des {
		if !de.IsDir() {
			continue
		}
		dir := filepath.Join(s.clientDir, de.Name())
		files := []string{}
		var latest int64
		if fes, err := os.ReadDir(dir); err == nil {
			for _, fe := range fes {
				files = append(files, fe.Name())
				if fi, e := fe.Info(); e == nil && fi.ModTime().Unix() > latest {
					latest = fi.ModTime().Unix()
				}
			}
		}
		out = append(out, ClientInfo{ClientID: de.Name(), LogDir: dir, Files: files, LatestEntry: latest})
	}
	return out
}

// resolve maps (clientID, file) to a path inside the tree, or ok=false.
func (s *Store) resolve(clientID, file string) (string, bool) {
	if clientID == "server" {
		if file == "" || file == filepath.Base(s.serverLog) {
			return s.serverLog, s.serverLog != ""
		}
		return "", false
	}
	if !safeName(clientID) || !safeName(file) {
		return "", false
	}
	return filepath.Join(s.clientDir, clientID, file), true
}

// Path exposes a resolved log path (for the SSE tailer).
func (s *Store) Path(clientID, file string) (string, bool) { return s.resolve(clientID, file) }

// Read returns up to `tail` lines of a log.
func (s *Store) Read(clientID, file string, tail int) ([]string, error) {
	p, ok := s.resolve(clientID, file)
	if !ok {
		return nil, ErrBadID
	}
	if tail <= 0 {
		tail = 200
	}
	if tail > 5000 {
		tail = 5000
	}
	return config.TailFile(p, tail), nil
}

// Prune deletes per-client log files older than maxAge and removes empty client
// dirs. Never touches the server log (that's the daemon's own janitor's job).
func (s *Store) Prune(maxAge time.Duration) {
	cutoff := time.Now().Add(-maxAge)
	des, _ := os.ReadDir(s.clientDir)
	for _, de := range des {
		if !de.IsDir() {
			continue
		}
		dir := filepath.Join(s.clientDir, de.Name())
		fes, _ := os.ReadDir(dir)
		remaining := 0
		for _, fe := range fes {
			p := filepath.Join(dir, fe.Name())
			if fi, err := os.Stat(p); err == nil && fi.ModTime().Before(cutoff) {
				_ = os.Remove(p)
			} else {
				remaining++
			}
		}
		if remaining == 0 {
			_ = os.Remove(dir)
		}
	}
}

// RunJanitor prunes per-client logs older than 7 days at boot then daily.
func (s *Store) RunJanitor(stop <-chan struct{}) {
	const maxAge = 7 * 24 * time.Hour
	s.Prune(maxAge)
	t := time.NewTicker(6 * time.Hour)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
			s.Prune(maxAge)
		}
	}
}
