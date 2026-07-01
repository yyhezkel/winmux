package files

// service.go — the HTTP surface for the Files API (/api/v2/files/*). Uses query
// params (?path=) rather than path segments so nested paths need no URL
// encoding and the traversal guard sees the raw value. Every route is behind
// the shared bearer auth (per-device scoping layers on in a later sprint).

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"path"
	"strconv"

	"winmux-server/internal/core"
)

// Service serves the Files API over a core.FilesProvider.
type Service struct {
	fp core.FilesProvider
}

// NewService wires the HTTP layer to a provider (LocalFiles in production, a
// mock in tests).
func NewService(fp core.FilesProvider) *Service {
	return &Service{fp: fp}
}

// RegisterRoutes mounts the /api/v2/files/* endpoints behind auth.
func (s *Service) RegisterRoutes(mux *http.ServeMux, auth func(http.HandlerFunc) http.HandlerFunc) {
	mux.HandleFunc("/api/v2/files/list", auth(s.handleList))
	mux.HandleFunc("/api/v2/files/read", auth(s.handleRead))
	mux.HandleFunc("/api/v2/files/upload", auth(s.handleUpload))
	mux.HandleFunc("/api/v2/files/delete", auth(s.handleDelete))
	mux.HandleFunc("/api/v2/files/download", auth(s.handleDownload))
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

// fail maps provider errors to HTTP status codes.
func fail(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, ErrOutsideSandbox):
		http.Error(w, err.Error(), http.StatusForbidden)
	case errors.Is(err, ErrNotFound):
		http.Error(w, err.Error(), http.StatusNotFound)
	case errors.Is(err, ErrIsDir):
		http.Error(w, err.Error(), http.StatusBadRequest)
	default:
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (s *Service) handleList(w http.ResponseWriter, r *http.Request) {
	depth := 1
	if v := r.URL.Query().Get("depth"); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			depth = n
		}
	}
	cwd, entries, err := s.fp.List(r.URL.Query().Get("path"), depth)
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, map[string]any{"cwd": cwd, "entries": entries})
}

func (s *Service) handleRead(w http.ResponseWriter, r *http.Request) {
	var maxBytes int64
	if v := r.URL.Query().Get("max_bytes"); v != "" {
		if n, err := strconv.ParseInt(v, 10, 64); err == nil {
			maxBytes = n
		}
	}
	data, truncated, err := s.fp.Read(r.URL.Query().Get("path"), maxBytes)
	if err != nil {
		fail(w, err)
		return
	}
	// Raw bytes so text + binary both work; truncation reported in a header.
	w.Header().Set("Content-Type", "application/octet-stream")
	if truncated {
		w.Header().Set("X-Winmux-Truncated", "true")
	}
	_, _ = w.Write(data)
}

func (s *Service) handleUpload(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "POST required", http.StatusMethodNotAllowed)
		return
	}
	dest := r.URL.Query().Get("path")
	if dest == "" {
		http.Error(w, "path query param required", http.StatusBadRequest)
		return
	}
	// Cap the request body so a huge upload can't exhaust memory; the provider
	// enforces the precise per-instance limit on the decoded bytes.
	r.Body = http.MaxBytesReader(w, r.Body, DefaultMaxUpload+(1<<20))
	if err := r.ParseMultipartForm(16 << 20); err != nil {
		http.Error(w, "bad multipart body: "+err.Error(), http.StatusBadRequest)
		return
	}
	file, _, err := r.FormFile("file")
	if err != nil {
		http.Error(w, "missing 'file' part", http.StatusBadRequest)
		return
	}
	defer file.Close()
	data, err := io.ReadAll(file)
	if err != nil {
		http.Error(w, "read upload: "+err.Error(), http.StatusBadRequest)
		return
	}
	sum, size, err := s.fp.Write(dest, data)
	if err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, map[string]any{"path": dest, "size": size, "sha256": sum})
}

func (s *Service) handleDelete(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodDelete && r.Method != http.MethodPost {
		http.Error(w, "DELETE required", http.StatusMethodNotAllowed)
		return
	}
	if err := s.fp.Delete(r.URL.Query().Get("path")); err != nil {
		fail(w, err)
		return
	}
	writeJSON(w, map[string]any{"ok": true})
}

func (s *Service) handleDownload(w http.ResponseWriter, r *http.Request) {
	p := r.URL.Query().Get("path")
	rc, size, err := s.fp.Open(p)
	if err != nil {
		fail(w, err)
		return
	}
	defer rc.Close()
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("Content-Length", strconv.FormatInt(size, 10))
	w.Header().Set("Content-Disposition", "attachment; filename=\""+path.Base(p)+"\"")
	_, _ = io.Copy(w, rc)
}
