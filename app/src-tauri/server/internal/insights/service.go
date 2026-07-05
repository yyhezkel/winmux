package insights

// service.go — the insights HTTP surface (metrics + docker + processes + logs +
// hygiene). Handlers moved verbatim from the old flat daemon's api.go; they call
// package-internal collectors directly (same package, so no export needed).

import (
	"encoding/json"
	"log"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"winmux-server/internal/config"
	"winmux-server/internal/core"
)

// splitDockerPath extracts {id}/action from either the legacy `/docker/…` path
// or the `/api/v2/insights/docker/…` path (both register handleDockerAction).
func splitDockerPath(p string) []string {
	const seg = "/docker/"
	i := strings.LastIndex(p, seg)
	if i < 0 {
		return nil
	}
	return strings.Split(p[i+len(seg):], "/")
}

// Service bundles the metrics sampler + store and serves the insights API.
type Service struct {
	store   *Store
	sm      *Sampler
	logPath string
}

// NewService wires a Service. The caller (cmd) owns the sampler/store lifecycle.
func NewService(store *Store, sm *Sampler, logPath string) *Service {
	return &Service{store: store, sm: sm, logPath: logPath}
}

// RegisterRoutes mounts the endpoints at BOTH their legacy paths (compat window,
// ≥3 minors) and the new /api/v2/* prefix, each behind auth.
func (s *Service) RegisterRoutes(mux *http.ServeMux, auth func(http.HandlerFunc) http.HandlerFunc) {
	pairs := []struct {
		legacy, v2 string
		h          http.HandlerFunc
	}{
		{"/current", "/api/v2/insights/current", s.handleCurrent},
		{"/history", "/api/v2/insights/history", s.handleHistory},
		{"/docker", "/api/v2/insights/docker", s.handleDocker},
		{"/docker/", "/api/v2/insights/docker/", s.handleDockerAction},
		{"/processes", "/api/v2/insights/processes", s.handleProcesses},
		{"/logs", "/api/v2/logs/daemon", s.handleLogs},
		{"/hygiene", "/api/v2/insights/hygiene", s.handleHygiene},
		{"/hygiene/kill", "/api/v2/insights/hygiene/kill", s.handleHygieneKill},
	}
	for _, p := range pairs {
		mux.HandleFunc(p.legacy, auth(p.h))
		mux.HandleFunc(p.v2, auth(p.h))
	}
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func (s *Service) handleCurrent(w http.ResponseWriter, _ *http.Request) {
	// Serve the freshest ticker-produced snapshot; never collect live in the
	// request path (Phase 72.3 — a blocking sample made the Monitor time out).
	writeJSON(w, s.sm.Current())
}

func (s *Service) handleHistory(w http.ResponseWriter, r *http.Request) {
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

func (s *Service) handleDocker(w http.ResponseWriter, _ *http.Request) {
	conts, err := dockerList()
	if err == nil {
		log.Printf("docker: ok — %d container(s)", len(conts))
		writeJSON(w, map[string]any{
			"available":      true,
			"containers":     conts,
			"socket":         dockerSockPath(),
			"daemon_version": core.Version,
		})
		return
	}
	socket, reason, detail := dockerResolve()
	if reason == "" {
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
		"daemon_version": core.Version,
		"containers":     []DockerContainer{},
	})
}

func (s *Service) handleDockerAction(w http.ResponseWriter, r *http.Request) {
	parts := splitDockerPath(r.URL.Path)
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

func (s *Service) handleProcesses(w http.ResponseWriter, r *http.Request) {
	limit := 20
	if v := r.URL.Query().Get("limit"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			limit = n
		}
	}
	writeJSON(w, map[string]any{"processes": topProcesses(limit)})
}

func (s *Service) handleHygiene(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, collectHygiene())
}

func (s *Service) handleHygieneKill(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Pids []int32 `json:"pids"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "bad body", http.StatusBadRequest)
		return
	}
	killed := killPids(body.Pids)
	log.Printf("hygiene: kill requested=%d killed=%d", len(body.Pids), len(killed))
	writeJSON(w, map[string]any{"killed": killed})
}

func (s *Service) handleLogs(w http.ResponseWriter, r *http.Request) {
	tail := 200
	if v := r.URL.Query().Get("tail"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			tail = n
		}
	}
	if tail > 2000 {
		tail = 2000
	}
	lines := config.TailFile(s.logPath, tail)
	writeJSON(w, map[string]any{"path": s.logPath, "lines": lines})
}
