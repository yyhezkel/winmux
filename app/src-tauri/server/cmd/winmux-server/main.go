// Command winmux-server is the winmux server daemon (Phase 77) — the former
// winmux-insights, restructured into internal/* subsystems behind core
// interfaces. S1.b wires the metrics side (insights + config + auth + api);
// chat/pairing/hooks join in S1.c.
package main

import (
	"context"
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
	"winmux-server/internal/workspace"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version", "-v", "version":
			// Same output shape as legacy `winmux-insights --version` so the
			// desktop's version probe keeps working across the rename.
			fmt.Printf("winmux-server %s\n", core.Version)
			return
		case "openapi":
			// Emit the generated OpenAPI spec to stdout and exit. The SDK
			// pipeline (sdk-gen/) and its CI drift-guard use this — no running
			// server or data dir needed (nil providers; handlers never run).
			// All subsystems present (nil-backed) so every SDK-facing op is in the
			// spec — registration only reflects types; no store is touched.
			srv := api.NewServer("", 0, api.Deps{
				Files:     files.NewService(nil),
				Logs:      logs.NewService(nil),
				Chat:      chat.NewChatAPI(nil, nil, ""),
				Workspace: workspace.NewService(workspace.NewManager(nil, nil), ""),
			})
			b, err := srv.OpenAPISpec()
			if err != nil {
				log.Fatalf("openapi: %v", err)
			}
			_, _ = os.Stdout.Write(b)
			return
		}
	}

	home, _ := os.UserHomeDir()
	// Phase 77 S5: the data dir is ~/.winmux/server. A 1.x install kept it at
	// ~/.winmux/insights; the daemon migrates it in place on first boot (below),
	// preserving token + chat.db (paired devices) + workspace/metrics DBs + logs.
	defBase := filepath.Join(home, ".winmux", "server")
	legacyBase := filepath.Join(home, ".winmux", "insights")

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

	// Migrate only when using the default dir (an explicit --dir opts out).
	if *base == defBase {
		if migrated, err := config.MigrateDataDir(legacyBase, defBase); err != nil {
			log.Printf("data-dir migration %s → %s failed: %v (continuing)", legacyBase, defBase, err)
		} else if migrated {
			log.Printf("migrated data dir %s → %s", legacyBase, defBase)
		}
	}

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

	// Workspace API (S3) — shared-state model (sessions + subscribers + pending)
	// in its own workspace.db.
	var wsSvc *workspace.Service
	if wstore, werr := workspace.OpenStore(filepath.Join(*base, "workspace.db")); werr != nil {
		log.Printf("workspace: open store failed, Workspace API disabled: %v", werr)
	} else {
		defer wstore.Close()
		wmgr := workspace.NewManager(wstore, nil) // NoopSender until FCM lands
		// Backward-compat: a stable "default" workspace so legacy chat sessions
		// (still served at /api/claude/*) have a home in the workspace model as
		// the deeper chat↔workspace merge lands in a later sprint.
		if _, e := wmgr.EnsureWorkspace(workspace.DefaultID, "default"); e != nil {
			log.Printf("workspace: ensure default failed: %v", e)
		}
		wsSvc = workspace.NewService(wmgr, token)
		// Accept paired-device tokens on the subscribe WS (matches the REST
		// surface) so a phone's long-term token works on the stream, not just
		// the shared desktop token.
		if chatAPI != nil {
			wsSvc.SetDeviceAuth(chatAPI.TokenValid)
		}
		log.Printf("workspace: API enabled")
	}

	srv := api.NewServer(token, *port, api.Deps{
		Insights: svc, Chat: chatAPI, Files: filesSvc, Logs: logsSvc, Workspace: wsSvc,
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
	log.Printf("winmux-server stopping (draining connections)")
	close(stop) // stop samplers/janitors/sweepers

	// Graceful HTTP drain so an in-flight metrics/file request isn't cut off on
	// restart; hard-stop after the deadline.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(ctx); err != nil {
		log.Printf("winmux-server: shutdown drain: %v", err)
	}
	log.Printf("winmux-server stopped")
}
