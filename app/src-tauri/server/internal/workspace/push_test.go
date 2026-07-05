package workspace

import (
	"encoding/json"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

// mockNotifier records Notify calls and signals each on ch (buffered).
type mockNotifier struct {
	mu    sync.Mutex
	calls []string
	ch    chan struct{}
}

func (m *mockNotifier) Notify(deviceID string, _ map[string]any) error {
	m.mu.Lock()
	m.calls = append(m.calls, deviceID)
	m.mu.Unlock()
	select {
	case m.ch <- struct{}{}:
	default:
	}
	return nil
}
func (m *mockNotifier) count() int { m.mu.Lock(); defer m.mu.Unlock(); return len(m.calls) }
func (m *mockNotifier) reset()     { m.mu.Lock(); m.calls = nil; m.mu.Unlock() }

type mockLister struct{ ids []string }

func (m mockLister) ActiveDeviceIDs() []string { return m.ids }

func newPushMgr(t *testing.T, notif *mockNotifier, ids []string) (*Manager, Session) {
	t.Helper()
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	m := NewManager(st, notif)
	m.SetPushLister(mockLister{ids: ids})
	ws, err := m.CreateWorkspace("t")
	if err != nil {
		t.Fatal(err)
	}
	se, err := m.CreateSession(ws.ID, "claude_chat")
	if err != nil {
		t.Fatal(err)
	}
	return m, se
}

// waitFor blocks until n signals arrive on ch or the deadline elapses.
func waitFor(t *testing.T, ch chan struct{}, n int) {
	t.Helper()
	deadline := time.After(2 * time.Second)
	for i := 0; i < n; i++ {
		select {
		case <-ch:
		case <-deadline:
			t.Fatalf("timed out waiting for signal %d/%d", i+1, n)
		}
	}
}

func TestMaybePushFanoutToActiveDevices(t *testing.T) {
	notif := &mockNotifier{ch: make(chan struct{}, 8)}
	m, se := newPushMgr(t, notif, []string{"dev_a", "dev_b"})

	// No subscribers + a hook_request → push to every active device.
	if _, err := m.Publish(se.ID, FrameHookRequest, json.RawMessage(`{"req_id":"r1"}`)); err != nil {
		t.Fatal(err)
	}
	waitFor(t, notif.ch, 2)
	if got := notif.count(); got != 2 {
		t.Fatalf("want 2 pushes, got %d", got)
	}
}

func TestMaybePushSkipsNonNotifyTypes(t *testing.T) {
	notif := &mockNotifier{ch: make(chan struct{}, 8)}
	m, se := newPushMgr(t, notif, []string{"dev_a"})

	// tool_use is a high-frequency stream frame — must NOT push.
	if _, err := m.Publish(se.ID, FrameToolUse, json.RawMessage(`{}`)); err != nil {
		t.Fatal(err)
	}
	time.Sleep(150 * time.Millisecond)
	if got := notif.count(); got != 0 {
		t.Fatalf("tool_use must not push, got %d", got)
	}
}

func TestMaybePushSuppressedByLiveSubscriber(t *testing.T) {
	notif := &mockNotifier{ch: make(chan struct{}, 8)}
	m, se := newPushMgr(t, notif, []string{"dev_a"})

	_, _, cancel, err := m.Subscribe(se.ID, "client1", "phone", 0)
	if err != nil {
		t.Fatal(err)
	}
	defer cancel()

	// A live WS subscriber is attached → the phone is awake, no push needed.
	if _, err := m.Publish(se.ID, FrameAssistantText, json.RawMessage(`{"content":"hi"}`)); err != nil {
		t.Fatal(err)
	}
	time.Sleep(150 * time.Millisecond)
	if got := notif.count(); got != 0 {
		t.Fatalf("push must be suppressed while a subscriber is live, got %d", got)
	}
}
