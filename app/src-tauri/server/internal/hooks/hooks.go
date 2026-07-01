// Package hooks is the thin TCP listener for the Phase-66 hook RPC. It owns
// none of the protocol: it binds a localhost port, reports the bound address to
// the handler (core.AddrSink) so spawned claude children can be pointed at it,
// and hands each accepted connection to a core.HookConnHandler — implemented by
// chat.SessionManager, which owns the per-session HMAC tokens + pending-hook
// state. This indirection is the concrete break of the Phase-69
// WS↔session↔hookRPC import cycle: hooks → core, chat → core, cmd wires them.
package hooks

import (
	"log"
	"net"

	"winmux-server/internal/core"
)

// Start binds an ephemeral localhost port and serves hook RPC connections for
// the life of the process. Best-effort: if the listen fails, hooks simply won't
// reach mobile (logged) and the rest of the server is unaffected.
func Start(h core.HookConnHandler) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		log.Printf("hooks: RPC listen failed, hooks won't reach mobile: %v", err)
		return
	}
	if sink, ok := h.(core.AddrSink); ok {
		sink.SetHookAddr(ln.Addr().String())
	}
	log.Printf("hooks: RPC listening on %s", ln.Addr().String())
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go h.HandleHookConn(conn)
		}
	}()
}
