// Package api is the HTTP front door: it owns the mux, unauthenticated liveness
// + version-negotiation endpoints, and mounts each subsystem's routes behind
// the auth middleware. Subsystems never import api; api imports them (no cycle).
package api

import (
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"winmux-server/internal/auth"
	"winmux-server/internal/core"
	"winmux-server/internal/insights"
)

// Server wires the subsystems into one HTTP listener. Chat is added in S1.c.
type Server struct {
	token    string
	port     int
	insights *insights.Service
}

// NewServer builds the front door. More subsystems join the signature as they
// migrate (chat in S1.c).
func NewServer(token string, port int, ins *insights.Service) *Server {
	return &Server{token: token, port: port, insights: ins}
}

// Handler builds the fully-wired mux (exported so tests can exercise routes via
// httptest without binding a port).
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	authMW := auth.Bearer(s.token)

	// Unauthenticated: liveness + version negotiation (PHASE-77-DESIGN §4).
	mux.HandleFunc("/healthz", s.handleHealth)
	mux.HandleFunc("/api/version", s.handleVersion)

	// Subsystems mount their own legacy + /api/v2 routes behind auth.
	s.insights.RegisterRoutes(mux, authMW)
	return mux
}

// Run serves until the listener errors.
func (s *Server) Run() error {
	srv := &http.Server{
		Addr:         fmt.Sprintf("127.0.0.1:%d", s.port),
		Handler:      s.Handler(),
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 20 * time.Second,
	}
	return srv.ListenAndServe()
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func (s *Server) handleHealth(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, map[string]any{"ok": true, "version": core.Version})
}

// handleVersion lets a client negotiate: which API majors + WS frame version
// this server speaks (PHASE-77-DESIGN §4, §4.4).
func (s *Server) handleVersion(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, map[string]any{
		"name":          "winmux-server",
		"version":       core.Version,
		"api_versions":  []int{2},
		"frame_version": core.FrameVersion,
	})
}
