// Command winmux-server is the winmux server daemon (Phase 77) — the former
// winmux-insights, restructured into internal/* subsystems behind core
// interfaces. Sprint 1 stands up the module skeleton; the metrics/chat/pairing
// subsystems are moved in and wired here over S1.b/S1.c.
package main

import (
	"fmt"
	"os"

	"winmux-server/internal/core"
)

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version", "-v", "version":
			// Same output shape as the legacy `winmux-insights --version` so the
			// desktop's version probe keeps working across the rename.
			fmt.Printf("winmux-server %s\n", core.Version)
			return
		}
	}
	// TODO(S1.b/c): parse flags, build config/insights/chat/pairing/hooks, wire
	// the api.Server + hooks.Listener, and run until SIGINT/SIGTERM.
	fmt.Fprintf(os.Stderr, "winmux-server %s — Phase 77 scaffold; serve wiring lands in S1.b/c\n", core.Version)
}
