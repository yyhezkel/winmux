// Package auth gates the winmux-server HTTP API. Sprint 1 ports the existing
// constant bearer-token check; per-client (device) scoping (PHASE-77-DESIGN
// §4.3) layers on here in later sprints.
package auth

import (
	"net/http"
	"strings"
)

// Bearer returns a middleware that rejects any request whose Authorization
// header isn't `Bearer <token>`. The port is localhost-only but forwarded over
// the winmux tunnel, so it stays token-gated.
func Bearer(token string) func(http.HandlerFunc) http.HandlerFunc {
	return func(h http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			got := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
			if got == "" || got != token {
				http.Error(w, "unauthorized", http.StatusUnauthorized)
				return
			}
			h(w, r)
		}
	}
}
