package insights

import "testing"

// Current() must publish a snapshot on the fallback path and then serve the
// cached pointer without re-sampling — the Phase 72.3 guarantee that /current
// never blocks the request goroutine on live collection.
func TestSamplerCurrentCachesAndReuses(t *testing.T) {
	s := NewSampler()
	if s.last.Load() != nil {
		t.Fatal("fresh sampler should have no cached snapshot")
	}

	// Empty cache → one live sample, published to `last`.
	got := s.Current()
	if got == nil {
		t.Fatal("Current returned nil")
	}
	cached := s.last.Load()
	if cached == nil {
		t.Fatal("Sample must publish to the last cache")
	}

	// Subsequent calls return the SAME cached pointer (no fresh sample).
	if a, b := s.Current(), s.Current(); a != b || a != cached {
		t.Fatalf("Current should reuse the cached snapshot pointer (a=%p b=%p cached=%p)", a, b, cached)
	}
}

// A ticker-style Sample(true) publishes a snapshot that Current then serves.
func TestSamplePublishesForCurrent(t *testing.T) {
	s := NewSampler()
	snap := s.Sample(true)
	if snap == nil {
		t.Fatal("Sample returned nil")
	}
	if s.Current() != snap {
		t.Fatal("Current should serve the most recently sampled snapshot")
	}
}
