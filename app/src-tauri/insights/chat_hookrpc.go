package main

// Phase 69.C — hook bridge (placeholder). Replaced with the real Phase 66
// RPC server (HMAC handshake + feed.push) in the 69.C milestone. Until then
// startHookRPC is a no-op: rpcAddr stays empty, so sessions spawn without the
// WINMUX_* hook env and the chat path is testable on its own.
func startHookRPC(_ *SessionManager) {}
