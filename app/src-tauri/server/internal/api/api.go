// Package api is the HTTP front door: it owns the mux, unauthenticated liveness
// + version-negotiation endpoints, and mounts each subsystem's routes behind
// the auth middleware. Subsystems never import api; api imports them (no cycle).
package api

import (
	_ "embed"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"winmux-server/internal/auth"
	"winmux-server/internal/chat"
	"winmux-server/internal/core"
	"winmux-server/internal/files"
	"winmux-server/internal/insights"
	"winmux-server/internal/logs"
	"winmux-server/internal/workspace"
)

// Deps is the set of subsystems the front door mounts. Any field may be nil to
// disable that subsystem.
type Deps struct {
	Insights  *insights.Service
	Chat      *chat.ChatAPI      // nil if chat disabled
	Files     *files.Service     // nil if files disabled
	Logs      *logs.Service      // nil if logs disabled
	Workspace *workspace.Service // nil if workspace disabled
}

// Server wires the subsystems into one HTTP listener.
type Server struct {
	token string
	port  int
	deps  Deps
}

// NewServer builds the front door.
func NewServer(token string, port int, deps Deps) *Server {
	return &Server{token: token, port: port, deps: deps}
}

// API specs served at /api/openapi.json + /api/asyncapi.json. Hand-authored in
// S2 (kept accurate to the handlers); huma auto-generation is scheduled for S4.
//
//go:embed openapi.json
var openapiSpec []byte

//go:embed asyncapi.json
var asyncapiSpec []byte

func serveSpec(spec []byte) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Cache-Control", "public, max-age=300")
		_, _ = w.Write(spec)
	}
}

// Handler builds the fully-wired mux (exported so tests can exercise routes via
// httptest without binding a port).
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	authMW := auth.Bearer(s.token)

	// Unauthenticated: liveness + version negotiation + API specs (§4, §4.4).
	mux.HandleFunc("/healthz", s.handleHealth)
	mux.HandleFunc("/api/version", s.handleVersion)
	mux.HandleFunc("/api/openapi.json", serveSpec(openapiSpec))
	mux.HandleFunc("/api/asyncapi.json", serveSpec(asyncapiSpec))

	// Subsystems mount their own legacy + /api/v2 routes behind auth.
	if s.deps.Insights != nil {
		s.deps.Insights.RegisterRoutes(mux, authMW)
	}
	if s.deps.Files != nil {
		s.deps.Files.RegisterRoutes(mux, authMW)
	}
	if s.deps.Logs != nil {
		s.deps.Logs.RegisterRoutes(mux, authMW)
	}
	if s.deps.Workspace != nil {
		s.deps.Workspace.RegisterRoutes(mux, authMW)
	}
	if s.deps.Chat != nil {
		// Chat brings its own auth (device tokens + shared bearer) and registers
		// its legacy /api/claude/* + /ws/claude/* routes. v2 chat aliases land in
		// a later sprint; legacy paths keep existing clients working (S1 compat).
		s.deps.Chat.RegisterRoutes(mux)
	}
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
