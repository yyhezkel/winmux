package main

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

type server struct {
	store *Store
	sm    *Sampler
	token string
	port  int
	chat  *chatAPI // Phase 69 — mobile Claude chat (nil if disabled)
}

func newServer(store *Store, sm *Sampler, token string, port int) *server {
	return &server{store: store, sm: sm, token: token, port: port}
}

func (s *server) run() error {
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", s.handleHealth) // unauthenticated liveness
	mux.HandleFunc("/current", s.auth(s.handleCurrent))
	mux.HandleFunc("/history", s.auth(s.handleHistory))
	mux.HandleFunc("/docker", s.auth(s.handleDocker))
	mux.HandleFunc("/docker/", s.auth(s.handleDockerAction)) // /docker/{id}/action
	mux.HandleFunc("/processes", s.auth(s.handleProcesses))
	if s.chat != nil {
		s.chat.registerRoutes(mux) // Phase 69 — /api/claude/* + /ws/claude/*
	}
	srv := &http.Server{
		Addr:         fmt.Sprintf("127.0.0.1:%d", s.port),
		Handler:      mux,
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 20 * time.Second,
	}
	return srv.ListenAndServe()
}

// auth — constant-ish bearer check (localhost-only, but token-gated since
// the port is forwarded over the winmux tunnel).
func (s *server) auth(h http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		got := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		if got == "" || got != s.token {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		h(w, r)
	}
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func (s *server) handleHealth(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, map[string]any{"ok": true, "version": Version})
}

func (s *server) handleCurrent(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, s.sm.Sample(true))
}

func (s *server) handleHistory(w http.ResponseWriter, r *http.Request) {
	metric := r.URL.Query().Get("metric")
	if metric == "" {
		metric = "cpu"
	}
	var since int64
	if v := r.URL.Query().Get("since"); v != "" {
		if n, err := strconv.ParseInt(v, 10, 64); err == nil {
			since = n
		} else if t, err := time.Parse(time.RFC3339, v); err == nil {
			since = t.Unix()
		}
	}
	if since == 0 {
		since = time.Now().Add(-time.Hour).Unix()
	}
	pts, err := s.store.history(metric, since, 0)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{"metric": metric, "points": pts})
}

func (s *server) handleDocker(w http.ResponseWriter, _ *http.Request) {
	conts, err := dockerList()
	if err == nil {
		log.Printf("docker: ok — %d container(s)", len(conts))
		writeJSON(w, map[string]any{
			"available":      true,
			"containers":     conts,
			"daemon_version": Version,
		})
		return
	}
	// Classify + log so a failing server self-explains in insights.log.
	socket, reason, detail := dockerResolve()
	if reason == "" {
		// Socket reachable but the API call still failed (daemon down, etc.).
		reason = "api_error"
		detail = err.Error()
	}
	logDockerUnavailable(socket, reason, detail)
	writeJSON(w, map[string]any{
		"available":      false,
		"reason":         reason,
		"detail":         detail,
		"socket":         socket,
		"hint":           dockerHint(reason),
		"uid":            os.Getuid(),
		"daemon_version": Version,
		"containers":     []DockerContainer{},
	})
}

func (s *server) handleDockerAction(w http.ResponseWriter, r *http.Request) {
	// path: /docker/{id}/action ; body: {"cmd":"start|stop|restart|kill"}
	parts := strings.Split(strings.TrimPrefix(r.URL.Path, "/docker/"), "/")
	if len(parts) < 2 || parts[1] != "action" {
		http.Error(w, "expected /docker/{id}/action", http.StatusBadRequest)
		return
	}
	var body struct {
		Cmd string `json:"cmd"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if err := dockerAction(parts[0], body.Cmd); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{"ok": true})
}

func (s *server) handleProcesses(w http.ResponseWriter, r *http.Request) {
	limit := 20
	if v := r.URL.Query().Get("limit"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			limit = n
		}
	}
	writeJSON(w, map[string]any{"processes": topProcesses(limit)})
}
