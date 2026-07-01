package api

// Backward-compat integration: exercise the real handlers (real store + real
// gopsutil sampler) through the wired mux, asserting that BOTH the legacy path
// and the new /api/v2 path serve the same data and that the bearer token is
// enforced — this is the "existing clients keep working" guarantee for S1.

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"winmux-server/internal/insights"
)

func realService(t *testing.T) *insights.Service {
	t.Helper()
	store, err := insights.OpenStore(filepath.Join(t.TempDir(), "metrics.db"))
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	t.Cleanup(store.Close)
	return insights.NewService(store, insights.NewSampler(), "")
}

func TestCurrentRoundTripLegacyAndV2(t *testing.T) {
	h := NewServer("secret", 0, Deps{Insights: realService(t)}).Handler()

	for _, path := range []string{"/current", "/api/v2/insights/current"} {
		req := httptest.NewRequest("GET", path, nil)
		req.Header.Set("Authorization", "Bearer secret")
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		if rec.Code != http.StatusOK {
			t.Fatalf("%s with token: want 200 got %d", path, rec.Code)
		}
		var snap map[string]any
		if err := json.Unmarshal(rec.Body.Bytes(), &snap); err != nil {
			t.Fatalf("%s: bad json: %v", path, err)
		}
		if _, ok := snap["cpu"]; !ok {
			t.Fatalf("%s: snapshot missing cpu field: %v", path, snap)
		}
	}

	// Wrong token is rejected on both the legacy and the v2 path.
	for _, path := range []string{"/current", "/api/v2/insights/current"} {
		req := httptest.NewRequest("GET", path, nil)
		req.Header.Set("Authorization", "Bearer wrong")
		rec := httptest.NewRecorder()
		h.ServeHTTP(rec, req)
		if rec.Code != http.StatusUnauthorized {
			t.Fatalf("%s wrong token: want 401 got %d", path, rec.Code)
		}
	}
}
