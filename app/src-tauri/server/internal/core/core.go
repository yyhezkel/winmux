// Package core holds the cross-subsystem interfaces and shared value types for
// winmux-server. It is a LEAF package: it imports no sibling internal package,
// so every subsystem depends on `core` and never on another subsystem. That is
// the concrete fix for the Phase-69 WS↔session↔hookRPC import cycle that forced
// the old daemon into one flat `package main` (see PHASE-77-DESIGN §15).
package core

import "net"

// Version is the winmux-server release version. Major 2 marks the API-stability
// guarantee introduced in Phase 77. One constant, shared by every package + cmd.
const Version = "2.0.0"

// FrameVersion is the WebSocket frame-contract version (PHASE-77-DESIGN §4.4).
// It is sent in the WS `hello` frame; a client that refuses an unknown value
// must fail loudly rather than silently drift.
const FrameVersion = 2

// HookConnHandler consumes a freshly-accepted hook-RPC connection and speaks the
// Phase-66 challenge/response + JSON-RPC protocol on it. It is implemented by
// chat's SessionManager (which owns the per-session HMAC tokens + pending-hook
// state) and driven by the thin hooks.Listener. This indirection is the cycle
// break: hooks → core, chat → core, and cmd wires the concrete handler in.
type HookConnHandler interface {
	HandleHookConn(conn net.Conn)
}

// AddrSink receives the hook listener's bound localhost address so the session
// manager can inject it (as WINMUX_SOCKET_ADDR) into spawned claude children.
type AddrSink interface {
	SetHookAddr(addr string)
}

// ─── Reserved for S3 (workspace shared state, Q3) ────────────────────────────
// Declared now to document the intended boundary; not yet consumed.

// EventBus is the fan-out surface for multi-client attach (use-case 8a) and
// notification broadcast (8b): many subscribers per session, every
// server-origin frame delivered to all of them. To be implemented across
// chat + workspace in Sprint 3.
type EventBus interface {
	Publish(sessionID string, frame []byte)
	Subscribe(sessionID string) (frames <-chan []byte, cancel func())
}
