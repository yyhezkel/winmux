// Command winmux-server is the winmux server daemon (Phase 77) — the former
// winmux-insights, restructured into internal/* subsystems behind core
// interfaces. S1.b wires the metrics side (insights + config + auth + api);
// chat/pairing/hooks join in S1.c.
package main

import (
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"winmux-server/internal/api"
	"winmux-server/internal/chat"
	"winmux-server/internal/config"
	"winmux-server/internal/core"
	"winmux-server/internal/files"
	"winmux-server/internal/hooks"
	"winmux-server/internal/insights"
	"winmux-server/internal/logs"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version", "-v", "version":
			// Same output shape as legacy `winmux-insights --version` so the
			// desktop's version probe keeps working across the rename.
			fmt.Printf("winmux-server %s\n", core.Version)
			return
		}
	}

	home, _ := os.UserHomeDir()
	// Backward compat: still read the existing ~/.winmux/insights data dir
	// (token, metrics.db) so an in-place 1.x→2.x upgrade preserves state. The
	// rename to ~/.winmux/server is an S1.d migration step.
	defBase := filepath.Join(home, ".winmux", "insights")

	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	port := fs.Int("port", 7879, "localhost TCP port for the API")
	base := fs.String("dir", defBase, "data directory (db, token, log)")
	interval := fs.Int("interval", 5, "sample interval, seconds")
	filesRoot := fs.String("files-root", home, "sandbox root for the Files API (default $HOME)")
	args := os.Args[1:]
	if len(args) > 0 && args[0] == "serve" {
		args = args[1:]
	}
	_ = fs.Parse(args)

	if err := os.MkdirAll(*base, 0o755); err != nil {
		log.Fatalf("mkdir %s: %v", *base, err)
	}

	logPath := filepath.Join(*base, "insights.log")
	config.RotateIfBig(logPath, 1<<20)
	lf, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err == nil {
		defer lf.Close()
		log.SetOutput(lf)
	}
	log.Printf("winmux-server %s starting (port=%d dir=%s interval=%ds)", core.Version, *port, *base, *interval)

	token := config.LoadOrCreateToken(filepath.Join(*base, "token"))

	store, err := insights.OpenStore(filepath.Join(*base, "metrics.db"))
	if err != nil {
		log.Fatalf("open store: %v", err)
	}
	defer store.Close()
	store.Sweep() // drop anything older than the retention window on boot

	sm := insights.NewSampler()
	stop := make(chan struct{})
	go insights.RunSampler(store, sm, time.Duration(*interval)*time.Second, stop)
	go config.LogJanitor(logPath, home, stop)
	go insights.PortWatchReaper(stop)

	svc := insights.NewService(store, sm, logPath)

	// Phase 69 — mobile Claude chat subsystem (separate chat.db). On any setup
	// error we log and continue serving metrics; chat just stays off.
	var chatAPI *chat.ChatAPI
	chatStore, cerr := chat.OpenChatStore(filepath.Join(*base, "chat.db"))
	if cerr != nil {
		log.Printf("chat: open store failed, chat disabled: %v", cerr)
	} else {
		defer chatStore.Close()
		mgr := chat.NewSessionManager(chatStore)
		chatAPI = chat.NewChatAPI(mgr, chatStore, token)
		hooks.Start(mgr) // thin listener → SessionManager.HandleHookConn (cycle-safe)
		go chat.RunSessionSweeper(mgr, stop)
		log.Printf("chat: Claude chat subsystem enabled")
	}

	// Files API (S2) — sandboxed to --files-root ($HOME by default).
	var filesSvc *files.Service
	if fp, ferr := files.NewLocalFiles(*filesRoot, 0); ferr != nil {
		log.Printf("files: init failed, Files API disabled: %v", ferr)
	} else {
		filesSvc = files.NewService(fp)
		log.Printf("files: API enabled (root=%s)", fp.Root())
	}

	// Logs API (S2) — per-client log tree under <dir>/logs, plus the "server"
	// pseudo-client that surfaces this daemon's own log.
	var logsSvc *logs.Service
	if lstore, lerr := logs.NewStore(*base, logPath); lerr != nil {
		log.Printf("logs: init failed, Logs API disabled: %v", lerr)
	} else {
		logsSvc = logs.NewService(lstore)
		go lstore.RunJanitor(stop)
		log.Printf("logs: API enabled")
	}

	srv := api.NewServer(token, *port, api.Deps{
		Insights: svc, Chat: chatAPI, Files: filesSvc, Logs: logsSvc,
	})

	go func() {
		if err := srv.Run(); err != nil {
			log.Fatalf("http server: %v", err)
		}
	}()
	log.Printf("API listening on 127.0.0.1:%d", *port)

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	close(stop)
	log.Printf("winmux-server stopping")
}
