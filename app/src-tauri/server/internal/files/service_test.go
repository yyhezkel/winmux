package files

import (
	"bytes"
	"encoding/json"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"testing"
)

// mount builds a mux with the files routes and a pass-through auth (auth itself
// is tested in the api package).
func mount(t *testing.T) http.Handler {
	t.Helper()
	lf, err := NewLocalFiles(t.TempDir(), 0)
	if err != nil {
		t.Fatal(err)
	}
	mux := http.NewServeMux()
	NewService(lf).RegisterRoutes(mux, func(h http.HandlerFunc) http.HandlerFunc { return h })
	return mux
}

func do(t *testing.T, h http.Handler, method, target string, body *bytes.Buffer, ct string) *httptest.ResponseRecorder {
	t.Helper()
	var r *http.Request
	if body == nil {
		r = httptest.NewRequest(method, target, nil)
	} else {
		r = httptest.NewRequest(method, target, body)
		r.Header.Set("Content-Type", ct)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, r)
	return rec
}

func TestFilesHTTPRoundTrip(t *testing.T) {
	h := mount(t)

	// upload via multipart to sub/note.txt
	var buf bytes.Buffer
	mw := multipart.NewWriter(&buf)
	fw, _ := mw.CreateFormFile("file", "note.txt")
	_, _ = fw.Write([]byte("hello mobile"))
	_ = mw.Close()
	rec := do(t, h, "POST", "/api/v2/files/upload?path=sub/note.txt", &buf, mw.FormDataContentType())
	if rec.Code != 200 {
		t.Fatalf("upload: got %d (%s)", rec.Code, rec.Body.String())
	}
	var up map[string]any
	_ = json.Unmarshal(rec.Body.Bytes(), &up)
	if up["sha256"] == "" || up["size"].(float64) != 12 {
		t.Fatalf("upload response unexpected: %v", up)
	}

	// list root depth=2 sees sub/note.txt
	rec = do(t, h, "GET", "/api/v2/files/list?path=&depth=2", nil, "")
	if rec.Code != 200 || !bytesContains(rec.Body.Bytes(), "sub/note.txt") {
		t.Fatalf("list: %d %s", rec.Code, rec.Body.String())
	}

	// read back
	rec = do(t, h, "GET", "/api/v2/files/read?path=sub/note.txt", nil, "")
	if rec.Code != 200 || rec.Body.String() != "hello mobile" {
		t.Fatalf("read: %d %q", rec.Code, rec.Body.String())
	}

	// download sets an attachment disposition
	rec = do(t, h, "GET", "/api/v2/files/download?path=sub/note.txt", nil, "")
	if rec.Code != 200 || rec.Header().Get("Content-Disposition") == "" || rec.Body.String() != "hello mobile" {
		t.Fatalf("download: %d disp=%q", rec.Code, rec.Header().Get("Content-Disposition"))
	}

	// delete, then read → 404
	rec = do(t, h, "DELETE", "/api/v2/files/delete?path=sub/note.txt", nil, "")
	if rec.Code != 200 {
		t.Fatalf("delete: %d", rec.Code)
	}
	rec = do(t, h, "GET", "/api/v2/files/read?path=sub/note.txt", nil, "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("read after delete: want 404 got %d", rec.Code)
	}
}

func bytesContains(b []byte, s string) bool { return bytes.Contains(b, []byte(s)) }
