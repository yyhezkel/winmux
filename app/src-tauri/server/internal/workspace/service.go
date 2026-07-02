package workspace

// service.go — REST surface for /api/v2/workspace/*. Uses Go 1.22 method +
// {id}/{sid} path patterns. The WS subscribe endpoint is added in S3.b.

import (
	"encoding/json"
	"errors"
	"net/http"
)

// Service serves the workspace REST + WS API over a Manager.
type Service struct {
	mgr   *Manager
	token string // bearer token (also accepted via ?token= on the WS route)
}

// NewService wires the HTTP layer to a Manager. token gates the subscribe WS
// (which can't always use the header); "" means open (tests).
func NewService(mgr *Manager, token string) *Service { return &Service{mgr: mgr, token: token} }

// RegisterRoutes mounts /api/v2/workspace/* — REST behind the shared auth
// middleware, and the subscribe WebSocket with its own header-or-query auth.
func (s *Service) RegisterRoutes(mux *http.ServeMux, auth func(http.HandlerFunc) http.HandlerFunc) {
	mux.HandleFunc("POST /api/v2/workspace/create", auth(s.handleCreate))
	mux.HandleFunc("GET /api/v2/workspace/{id}/sessions", auth(s.handleListSessions))
	mux.HandleFunc("GET /api/v2/workspace/{id}/session/{sid}/subscribe", s.handleSubscribe)
	mux.HandleFunc("GET /api/v2/workspace/{id}", auth(s.handleGet))
	mux.HandleFunc("DELETE /api/v2/workspace/{id}", auth(s.handleDelete))
	// Phase 77 S6: list, get-session, and create-session are served as typed
	// huma ops (api package) so they land in the generated OpenAPI + SDKs — the
	// mobile-consumed surface. Their raw handlers (handleList / handleGetSession
	// / handleCreateSession) remain the reference impl the huma ops delegate to.
}

// Mgr exposes the manager so the api package's huma ops can reuse the exact
// business logic behind the mobile-facing endpoints.
func (s *Service) Mgr() *Manager { return s.mgr }

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func fail(w http.ResponseWriter, err error) {
	if errors.Is(err, ErrNotFound) {
		http.Error(w, err.Error(), http.StatusNotFound)
		return
	}
	http.Error(w, err.Error(), http.StatusInternalServerError)
}

func (s *Service) handleCreate(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name string `json:"name"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	ws, err := s.mgr.CreateWorkspace(body.Name)
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, ws)
}

func (s *Service) handleGet(w http.ResponseWriter, r *http.Request) {
	ws, err := s.mgr.GetWorkspace(r.PathValue("id"))
	if err != nil {
		fail(w, err)
		return
	}
	sess, _ := s.mgr.ListSessions(ws.ID)
	writeJSON(w, map[string]any{
		"id": ws.ID, "name": ws.Name, "created_at": ws.CreatedAt, "sessions": sess,
	})
}

func (s *Service) handleDelete(w http.ResponseWriter, r *http.Request) {
	if err := s.mgr.DeleteWorkspace(r.PathValue("id")); err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, map[string]any{"ok": true})
}

func (s *Service) handleListSessions(w http.ResponseWriter, r *http.Request) {
	wsID := r.PathValue("id")
	if _, err := s.mgr.GetWorkspace(wsID); err != nil {
		fail(w, err)
		return
	}
	sess, _ := s.mgr.ListSessions(wsID)
	out := make([]map[string]any, 0, len(sess))
	for _, se := range sess {
		out = append(out, map[string]any{
			"id": se.ID, "kind": se.Kind, "last_activity": se.LastActivity,
			"subscribers_count": s.mgr.SubscriberCount(se.ID),
		})
	}
	writeJSON(w, out)
}

