// Package api is the HTTP front door: it owns the mux, unauthenticated liveness
// + version-negotiation endpoints, and mounts each subsystem's routes behind
// the auth middleware. Subsystems never import api; api imports them (no cycle).
package api

import (
	_ "embed"
	"fmt"
	"log"
	"net/http"
	"time"

	"winmux-server/internal/auth"
	"winmux-server/internal/chat"
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

// asyncapi.json is the hand-authored streaming (WebSocket) contract — OpenAPI
// doesn't describe WS frames, so the frame schema lives here (PHASE-77-DESIGN
// §4.4). The REST openapi.json is now generated from the huma handlers (S4).
//
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

	// The huma-hosted surface: liveness + version negotiation (public) and the
	// Files + Logs operations (bearer-gated by huma middleware). huma reflects
	// these into the OpenAPI we serve below, so the spec tracks the handlers.
	hapi := s.newHumaAPI(mux)
	spec, err := hapi.OpenAPI().MarshalJSON()
	if err != nil {
		log.Printf("api: OpenAPI marshal failed: %v", err) // serve an empty doc rather than crash
		spec = []byte("{}")
	}
	mux.HandleFunc("/api/openapi.json", serveSpec(spec))
	mux.HandleFunc("/api/asyncapi.json", serveSpec(asyncapiSpec))

	// Insights keeps its raw legacy + /api/v2 stdlib handlers behind auth
	// (desktop Monitor surface — not part of the generated SDK spec).
	if s.deps.Insights != nil {
		s.deps.Insights.RegisterRoutes(mux, authMW)
	}
	// Files + Logs are already registered on the huma API above.
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
