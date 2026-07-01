package logs

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestSafeNameRejectsTraversal(t *testing.T) {
	for _, bad := range []string{"", "..", ".", "../etc", "a/b", "a\\b", "a b", "x/../y"} {
		if safeName(bad) {
			t.Fatalf("safeName(%q) should be false", bad)
		}
	}
	for _, ok := range []string{"dev_abc123", "access.log", "requests-1.log", "server"} {
		if !safeName(ok) {
			t.Fatalf("safeName(%q) should be true", ok)
		}
	}
}

func TestAppendReadAndList(t *testing.T) {
	dir := t.TempDir()
	serverLog := filepath.Join(dir, "insights.log")
	_ = os.WriteFile(serverLog, []byte("boot line\n"), 0o644)

	st, err := NewStore(dir, serverLog)
	if err != nil {
		t.Fatal(err)
	}
	st.Append("dev_phone1", "access.log", "GET /current")
	st.Append("dev_phone1", "access.log", "GET /docker")
	// A traversal id must be silently dropped, creating nothing.
	st.Append("../evil", "x.log", "nope")

	lines, err := st.Read("dev_phone1", "access.log", 10)
	if err != nil || len(lines) != 2 {
		t.Fatalf("read: lines=%v err=%v", lines, err)
	}

	// server pseudo-client resolves to the daemon log.
	slines, err := st.Read("server", "", 10)
	if err != nil || len(slines) != 1 {
		t.Fatalf("server read: %v err=%v", slines, err)
	}

	clients := st.ListClients()
	var sawServer, sawPhone, sawEvil bool
	for _, c := range clients {
		switch c.ClientID {
		case "server":
			sawServer = true
		case "dev_phone1":
			sawPhone = true
		case "../evil", "evil":
			sawEvil = true
		}
	}
	if !sawServer || !sawPhone || sawEvil {
		t.Fatalf("clients unexpected: %+v", clients)
	}

	// bad id via the API path is a 400, not a traversal.
	if _, err := st.Read("../etc", "passwd", 10); err != ErrBadID {
		t.Fatalf("bad id: want ErrBadID got %v", err)
	}
}

func TestLogsHTTP(t *testing.T) {
	dir := t.TempDir()
	st, _ := NewStore(dir, filepath.Join(dir, "insights.log"))
	st.Append("dev_x", "requests.log", "hello")
	mux := http.NewServeMux()
	NewService(st).RegisterRoutes(mux, func(h http.HandlerFunc) http.HandlerFunc { return h })

	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequest("GET", "/api/v2/logs/list", nil))
	if rec.Code != 200 {
		t.Fatalf("list: %d", rec.Code)
	}
	var out struct {
		Clients []ClientInfo `json:"clients"`
	}
	_ = json.Unmarshal(rec.Body.Bytes(), &out)
	if len(out.Clients) == 0 {
		t.Fatal("list returned no clients")
	}

	rec = httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequest("GET", "/api/v2/logs/read?client_id=dev_x&file=requests.log&tail=5", nil))
	if rec.Code != 200 {
		t.Fatalf("read: %d %s", rec.Code, rec.Body.String())
	}

	// invalid id → 400
	rec = httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequest("GET", "/api/v2/logs/read?client_id=../x&file=y", nil))
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("bad id: want 400 got %d", rec.Code)
	}
}
