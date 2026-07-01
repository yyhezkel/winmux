package logs

// service.go — HTTP surface for the Logs API (/api/v2/logs/*): list clients,
// read a tail, and SSE-stream new lines. All behind the shared bearer auth.

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"strconv"
	"time"
)

// Service serves the Logs API over a Store.
type Service struct {
	store *Store
}

// NewService wires the HTTP layer to a Store.
func NewService(store *Store) *Service { return &Service{store: store} }

// RegisterRoutes mounts /api/v2/logs/{list,read,stream} behind auth.
// (The daemon's own log also stays at /api/v2/logs/daemon via insights.)
func (s *Service) RegisterRoutes(mux *http.ServeMux, auth func(http.HandlerFunc) http.HandlerFunc) {
	mux.HandleFunc("/api/v2/logs/list", auth(s.handleList))
	mux.HandleFunc("/api/v2/logs/read", auth(s.handleRead))
	mux.HandleFunc("/api/v2/logs/stream", auth(s.handleStream))
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func (s *Service) handleList(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, map[string]any{"clients": s.store.ListClients()})
}

func (s *Service) handleRead(w http.ResponseWriter, r *http.Request) {
	tail := 200
	if v := r.URL.Query().Get("tail"); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			tail = n
		}
	}
	lines, err := s.store.Read(r.URL.Query().Get("client_id"), r.URL.Query().Get("file"), tail)
	if err != nil {
		if errors.Is(err, ErrBadID) {
			http.Error(w, err.Error(), http.StatusBadRequest)
		} else {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		return
	}
	// truncated is true when we returned exactly the cap (older lines dropped).
	writeJSON(w, map[string]any{"lines": lines, "truncated": len(lines) >= 5000})
}

// handleStream is a tail -f over SSE: seek to EOF, then poll for appended lines
// and push each as `event: line`. Ends when the client disconnects.
func (s *Service) handleStream(w http.ResponseWriter, r *http.Request) {
	p, ok := s.store.Path(r.URL.Query().Get("client_id"), r.URL.Query().Get("file"))
	if !ok {
		http.Error(w, ErrBadID.Error(), http.StatusBadRequest)
		return
	}
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	f, err := os.Open(p)
	if err != nil {
		http.Error(w, "open log", http.StatusNotFound)
		return
	}
	defer f.Close()
	_, _ = f.Seek(0, io.SeekEnd) // start from new lines only

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	flusher.Flush()

	reader := bufio.NewReader(f)
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()
	ctx := r.Context()
	for {
		// Drain any complete lines currently available.
		for {
			line, err := reader.ReadString('\n')
			if len(line) > 0 {
				payload, _ := json.Marshal(map[string]string{"line": line[:len(line)-1]})
				_, _ = w.Write([]byte("event: line\ndata: "))
				_, _ = w.Write(payload)
				_, _ = w.Write([]byte("\n\n"))
				flusher.Flush()
			}
			if err != nil {
				break // EOF or partial line — wait for more
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
