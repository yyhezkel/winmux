// winmux-insights — a tiny server-metrics daemon (Phase 68.C).
//
// Runs on the user's remote server, sampled by winmux's desktop Monitor
// over the existing reverse SSH tunnel. Collects CPU / RAM / disk / network
// + Docker stats, stores a rolling 7 days in SQLite, and serves a
// localhost-only HTTP API (bearer-token auth).
//
// Design goals: <50 MB RAM, <1% CPU avg. Sampling is cheap (gopsutil); the
// SQLite writes are batched one tx per tick; Docker is polled on the same
// tick only if the socket is reachable. Pure-Go (CGO-free, modernc sqlite)
// so it cross-compiles to linux-x64 / linux-arm64 from any host.
package main

import (
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

const Version = "1.2.7"

func main() {
	if len(os.Args) > 1 && (os.Args[1] == "--version" || os.Args[1] == "-v" || os.Args[1] == "version") {
		fmt.Printf("winmux-insights %s\n", Version)
		return
	}

	home, _ := os.UserHomeDir()
	defBase := filepath.Join(home, ".winmux", "insights")

	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	port := fs.Int("port", 7879, "localhost TCP port for the API")
	base := fs.String("dir", defBase, "data directory (db, token, log)")
	interval := fs.Int("interval", 5, "sample interval, seconds")
	// Allow `serve` as an optional first arg.
	args := os.Args[1:]
	if len(args) > 0 && args[0] == "serve" {
		args = args[1:]
	}
	_ = fs.Parse(args)

	if err := os.MkdirAll(*base, 0o755); err != nil {
		log.Fatalf("mkdir %s: %v", *base, err)
	}

	// Log to a rotated-ish file (size-capped on open) + stderr.
	logPath := filepath.Join(*base, "insights.log")
	rotateIfBig(logPath, 1<<20)
	lf, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err == nil {
		defer lf.Close()
		log.SetOutput(lf)
	}
	log.Printf("winmux-insights %s starting (port=%d dir=%s interval=%ds)", Version, *port, *base, *interval)

	token := loadOrCreateToken(filepath.Join(*base, "token"))

	store, err := openStore(filepath.Join(*base, "metrics.db"))
	if err != nil {
		log.Fatalf("open store: %v", err)
	}
	defer store.Close()
	store.sweep() // drop anything older than the retention window on boot

	sm := newSampler()
	stop := make(chan struct{})
	go runSampler(store, sm, time.Duration(*interval)*time.Second, stop)
	go logJanitor(logPath, home, stop) // Phase 75: bound all server-side logs
	go portWatchReaper(stop)           // Phase 76.1: auto-kill duplicate port-watchers

	srv := newServer(store, sm, token, *port, logPath)

	// Phase 69 — mobile Claude chat subsystem (separate chat.db). On any
	// setup error we log and continue serving metrics; chat just stays off.
	chatStore, err := openChatStore(filepath.Join(*base, "chat.db"))
	if err != nil {
		log.Printf("chat: open store failed, chat disabled: %v", err)
	} else {
		defer chatStore.Close()
		mgr := newSessionManager(chatStore)
		srv.chat = newChatAPI(mgr, chatStore, token)
		startHookRPC(mgr) // 69.C — Phase 66 hook bridge listener
		go runSessionSweeper(mgr, stop)
		log.Printf("chat: Claude chat subsystem enabled (claude=%s)", mgr.cfg.claudeBin)
	}
	go func() {
		if err := srv.run(); err != nil {
			log.Fatalf("http server: %v", err)
		}
	}()
	log.Printf("API listening on 127.0.0.1:%d", *port)

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	close(stop)
	log.Printf("winmux-insights stopping")
}

// rotateIfBig renames the log to .1 when it exceeds max bytes (cheap, no
// external rotation dep). Used at boot, before the log fd is opened.
func rotateIfBig(path string, max int64) {
	if fi, err := os.Stat(path); err == nil && fi.Size() > max {
		_ = os.Rename(path, path+".1")
	}
}

// rotateCopyTruncate bounds a log WITHOUT breaking an open append fd: it copies
// the current contents to <path>.1 then truncates the original in place. Safe
// for a file the daemon (or the hook CLI) holds open with O_APPEND — unlike a
// rename, which would leave writers appending to the rotated-away inode.
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

// logJanitor keeps EVERY winmux server-side log bounded so they can't
// accumulate — the server-side mirror of the desktop's debug.log hygiene
// (Phase 75). The daemon is the natural janitor: it's the one long-running
// process, and every log lives under the same user's ~/.winmux tree. Runs at
// boot then every 30 min: size-caps insights.log (its own), the hook CLI's
// hook-debug.log, and mobile-install.log via copy-truncate, and age-prunes any
// that have gone stale.
func logJanitor(insightsLog, home string, stop <-chan struct{}) {
	const sizeCap = 1 << 20            // 1 MB
	const maxAge = 7 * 24 * time.Hour // delete stale logs after a week
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

func loadOrCreateToken(path string) string {
	if b, err := os.ReadFile(path); err == nil {
		if t := strings.TrimSpace(string(b)); t != "" {
			return t
		}
	}
	t := randHex(32)
	_ = os.WriteFile(path, []byte(t+"\n"), 0o600)
	return t
}
