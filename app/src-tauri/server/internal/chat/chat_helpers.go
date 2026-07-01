package chat

// Phase 69 — small shared helpers for the chat subsystem.

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"

	"winmux-server/internal/config"
)

// randHex delegates to config so the whole codebase shares one token generator.
func randHex(n int) string { return config.RandHex(n) }

// writeJSON marshals v as the JSON response body (chat's copy — each package
// owns its tiny HTTP helpers so there are no cross-package handler utilities).
func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

// hashToken returns the sha256 hex of a bearer token (devices store only the
// hash — Rule #2, no plaintext secrets at rest).
func hashToken(tok string) string {
	sum := sha256.Sum256([]byte(tok))
	return hex.EncodeToString(sum[:])
}

// jsonEvent marshals a map to a compact JSON byte slice. Marshalling a
// map[string]any never fails in practice; on the impossible error we return
// a minimal valid object rather than panicking the daemon.
func jsonEvent(m map[string]any) []byte {
	b, err := json.Marshal(m)
	if err != nil {
		return []byte(`{"type":"error","message":"marshal failed"}`)
	}
	return b
}

// chatErr is a typed error so handlers can map it to the right HTTP status.
type chatErr struct {
	msg  string
	kind string // "rate" | "state" | "notfound"
}

func (e *chatErr) Error() string { return e.msg }

func errRate(m string) error  { return &chatErr{msg: m, kind: "rate"} }
func errState(m string) error { return &chatErr{msg: m, kind: "state"} }
func errNotFound(m string) error {
	return &chatErr{msg: m, kind: "notfound"}
}

// failPendingHooks unblocks every parked hook waiter with a "deny" verdict.
// Called when the session dies so a hook RPC never hangs forever (69.C).
func (s *Session) failPendingHooks() {
	s.hookMu.Lock()
	for id, ch := range s.pendingHooks {
		select {
		case ch <- "deny":
		default:
		}
		delete(s.pendingHooks, id)
	}
	s.hookMu.Unlock()
}

// resolveHook delivers a mobile client's allow/deny to the parked hook RPC
// waiter (69.C). Unknown req_ids are ignored (already resolved or expired).
func (s *Session) resolveHook(reqID, decision string) {
	if decision != "allow" {
		decision = "deny"
	}
	s.hookMu.Lock()
	ch, ok := s.pendingHooks[reqID]
	if ok {
		delete(s.pendingHooks, reqID)
	}
	s.hookMu.Unlock()
	if ok {
		select {
		case ch <- decision:
		default:
		}
	}
}
