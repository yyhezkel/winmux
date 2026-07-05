package workspace

import (
	"encoding/json"
	"path/filepath"
	"testing"
)

// A desktop-forwarded hook (B path) publishes on a virtual session with no
// subscriber, so it fans out to paired devices; the desktop then polls
// HookResolution for the phone's decision.
func TestForwardHookPushesThenResolves(t *testing.T) {
	notif := &mockNotifier{ch: make(chan struct{}, 8)}
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	m := NewManager(st, notif)
	m.SetPushLister(mockLister{ids: []string{"dev_a"}})

	if err := m.ForwardHook(
		"desktop-ws1-pane1", "req-1",
		json.RawMessage(`{"req_id":"req-1","origin":"desktop","tool_name":"Bash"}`), 0,
	); err != nil {
		t.Fatalf("ForwardHook: %v", err)
	}
	waitFor(t, notif.ch, 1)
	if got := notif.count(); got != 1 {
		t.Fatalf("want 1 push for the forwarded hook, got %d", got)
	}

	// Unresolved until a winner-takes-all decision lands.
	if _, resolved := m.HookResolution("req-1"); resolved {
		t.Fatal("hook should be unresolved before any decision")
	}
	won, err := m.ResolveHook("req-1", "dev_a", "allow")
	if err != nil || !won {
		t.Fatalf("first resolve should win: won=%v err=%v", won, err)
	}
	dec, resolved := m.HookResolution("req-1")
	if !resolved || dec != "allow" {
		t.Fatalf("HookResolution = %q resolved=%v, want allow/true", dec, resolved)
	}
}
