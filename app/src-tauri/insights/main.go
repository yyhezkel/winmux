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

const Version = "1.0.0"

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

	srv := newServer(store, sm, token, *port)
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
// external rotation dep).
func rotateIfBig(path string, max int64) {
	if fi, err := os.Stat(path); err == nil && fi.Size() > max {
		_ = os.Rename(path, path+".1")
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
