package api

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"winmux-server/internal/insights"
)

// A Server whose insights service has no store/sampler — fine for testing
// routing + auth, since /healthz and /api/version don't touch them and an
// unauthorized request is rejected before any handler runs.
func testServer() *Server {
	return NewServer("secret", 0, insights.NewService(nil, nil, ""), nil)
}

func TestHealthAndVersionAreUnauthed(t *testing.T) {
	h := testServer().Handler()
	for _, path := range []string{"/healthz", "/api/version"} {
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, httptest.NewRequest("GET", path, nil))
		if rec.Code != http.StatusOK {
			t.Fatalf("%s: want 200 got %d", path, rec.Code)
		}
	}
	// /api/version advertises the negotiation shape.
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest("GET", "/api/version", nil))
	var v map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &v); err != nil {
		t.Fatal(err)
	}
	if v["name"] != "winmux-server" || v["api_versions"] == nil || v["frame_version"] == nil {
		t.Fatalf("version payload missing negotiation fields: %v", v)
	}
}

// Backward compat: both the legacy path and the new /api/v2 path are registered
// and both require the bearer token.
func TestMetricsRoutesRequireToken(t *testing.T) {
	h := testServer().Handler()
	for _, path := range []string{
		"/current", "/api/v2/insights/current",
		"/hygiene", "/api/v2/insights/hygiene",
	} {
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, httptest.NewRequest("GET", path, nil))
		if rec.Code != http.StatusUnauthorized {
			t.Fatalf("%s without token: want 401 got %d", path, rec.Code)
		}
	}
}
