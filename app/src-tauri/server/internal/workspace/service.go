package workspace

// service.go — REST surface for /api/v2/workspace/*. Uses Go 1.22 method +
// {id}/{sid} path patterns. The WS subscribe endpoint is added in S3.b.

import (
	"encoding/json"
	"errors"
	"net/http"
)

// Service serves the workspace REST API over a Manager.
type Service struct {
	mgr *Manager
}

// NewService wires the HTTP layer to a Manager.
func NewService(mgr *Manager) *Service { return &Service{mgr: mgr} }

// RegisterRoutes mounts /api/v2/workspace/* behind auth.
func (s *Service) RegisterRoutes(mux *http.ServeMux, auth func(http.HandlerFunc) http.HandlerFunc) {
	mux.HandleFunc("GET /api/v2/workspace/list", auth(s.handleList))
	mux.HandleFunc("POST /api/v2/workspace/create", auth(s.handleCreate))
	mux.HandleFunc("GET /api/v2/workspace/{id}/sessions", auth(s.handleListSessions))
	mux.HandleFunc("POST /api/v2/workspace/{id}/sessions", auth(s.handleCreateSession))
	mux.HandleFunc("GET /api/v2/workspace/{id}/session/{sid}", auth(s.handleGetSession))
	mux.HandleFunc("GET /api/v2/workspace/{id}", auth(s.handleGet))
	mux.HandleFunc("DELETE /api/v2/workspace/{id}", auth(s.handleDelete))
}

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

func (s *Service) handleList(w http.ResponseWriter, _ *http.Request) {
	wss, err := s.mgr.ListWorkspaces()
	if err != nil {
		fail(w, err)
		return
	}
	out := make([]map[string]any, 0, len(wss))
	for _, ws := range wss {
		sess, _ := s.mgr.ListSessions(ws.ID)
		out = append(out, map[string]any{
			"id": ws.ID, "name": ws.Name, "created_at": ws.CreatedAt,
			"active_session_count": len(sess),
		})
	}
	writeJSON(w, out)
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

func (s *Service) handleCreateSession(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Kind string `json:"kind"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	se, err := s.mgr.CreateSession(r.PathValue("id"), body.Kind)
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, map[string]any{"session_id": se.ID, "kind": se.Kind})
}

func (s *Service) handleGetSession(w http.ResponseWriter, r *http.Request) {
	se, err := s.mgr.GetSession(r.PathValue("sid"))
	if err != nil {
		fail(w, err)
		return
	}
	pending, _ := s.mgr.ListPending(se.ID)
	writeJSON(w, map[string]any{
		"id": se.ID, "kind": se.Kind, "workspace_id": se.WorkspaceID,
		"subscribers":      s.mgr.SubscriberCount(se.ID),
		"pending_requests": pending,
		"event_count":      s.mgr.store.EventCount(se.ID),
	})
}
