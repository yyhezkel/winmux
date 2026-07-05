package push

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

// memStore is an in-memory Deps backend for the push tests.
type memStore struct {
	mu     sync.Mutex
	seq    map[string]int64
	q      map[string][]QueuedEvent
	tokens map[string]string // token → deviceID
}

func newMemStore() *memStore {
	return &memStore{seq: map[string]int64{}, q: map[string][]QueuedEvent{}, tokens: map[string]string{}}
}

func (m *memStore) pending(dev string) int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.q[dev])
}

func (m *memStore) deps() Deps {
	return Deps{
		ResolveToken: func(tok string) (string, bool, bool) {
			m.mu.Lock()
			defer m.mu.Unlock()
			if d, ok := m.tokens[tok]; ok {
				return d, false, true
			}
			return "", false, false
		},
		Enqueue: func(dev, ev string, _ int) (int64, error) {
			m.mu.Lock()
			defer m.mu.Unlock()
			m.seq[dev]++
			s := m.seq[dev]
			m.q[dev] = append(m.q[dev], QueuedEvent{PushSeq: s, Ts: time.Now().Unix(), Event: ev})
			return s, nil
		},
		PendingAfter: func(dev string, cursor int64) []QueuedEvent {
			m.mu.Lock()
			defer m.mu.Unlock()
			var out []QueuedEvent
			for _, e := range m.q[dev] {
				if e.PushSeq > cursor {
					out = append(out, e)
				}
			}
			return out
		},
		Ack: func(dev string, upto int64) {
			m.mu.Lock()
			defer m.mu.Unlock()
			var keep []QueuedEvent
			for _, e := range m.q[dev] {
				if e.PushSeq > upto {
					keep = append(keep, e)
				}
			}
			m.q[dev] = keep
		},
	}
}

func dialURL(hs *httptest.Server, query string) string {
	return "ws" + strings.TrimPrefix(hs.URL, "http") + "/api/v2/push/subscribe?" + query
}

func readFrame(t *testing.T, c *websocket.Conn) map[string]any {
	t.Helper()
	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	var m map[string]any
	if err := c.ReadJSON(&m); err != nil {
		t.Fatalf("read frame: %v", err)
	}
	return m
}

func TestPushRejectsBadToken(t *testing.T) {
	st := newMemStore()
	hs := httptest.NewServer(http.HandlerFunc(New(st.deps()).Handler))
	defer hs.Close()
	_, resp, err := websocket.DefaultDialer.Dial(dialURL(hs, "token=nope"), nil)
	if err == nil {
		t.Fatal("expected dial to fail with 401")
	}
	if resp == nil || resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("want 401, got %v", resp)
	}
}

func TestPushLiveDeliveryAndAck(t *testing.T) {
	st := newMemStore()
	st.tokens["tok-a"] = "dev_a"
	srv := New(st.deps())
	hs := httptest.NewServer(http.HandlerFunc(srv.Handler))
	defer hs.Close()

	c, _, err := websocket.DefaultDialer.Dial(dialURL(hs, "token=tok-a&cursor=0"), nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()

	if hello := readFrame(t, c); hello["type"] != "hello" {
		t.Fatalf("want hello, got %v", hello)
	}

	if err := srv.Notify("dev_a", map[string]any{"type": "hook_request", "req_id": "r1"}); err != nil {
		t.Fatalf("notify: %v", err)
	}
	env := readFrame(t, c)
	if env["type"] != "event" {
		t.Fatalf("want event, got %v", env)
	}
	ev, _ := env["event"].(map[string]any)
	if ev["req_id"] != "r1" {
		t.Fatalf("wrong event payload: %v", ev)
	}
	seq := env["push_seq"].(float64)

	// Ack → the queue drains.
	if err := c.WriteJSON(map[string]any{"type": "ack", "push_seq": int64(seq)}); err != nil {
		t.Fatalf("ack: %v", err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for st.pending("dev_a") != 0 {
		if time.Now().After(deadline) {
			t.Fatalf("queue not drained after ack: %d left", st.pending("dev_a"))
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestPushReplaysQueueOnReconnect(t *testing.T) {
	st := newMemStore()
	st.tokens["tok-a"] = "dev_a"
	srv := New(st.deps())
	hs := httptest.NewServer(http.HandlerFunc(srv.Handler))
	defer hs.Close()

	// Event arrives while the device is offline (no connection): it's queued.
	if err := srv.Notify("dev_a", map[string]any{"type": "assistant_text", "content": "hi"}); err != nil {
		t.Fatalf("notify: %v", err)
	}
	if st.pending("dev_a") != 1 {
		t.Fatalf("want 1 queued, got %d", st.pending("dev_a"))
	}

	// Connect with cursor=0 → the queued event replays after hello.
	c, _, err := websocket.DefaultDialer.Dial(dialURL(hs, "token=tok-a&cursor=0"), nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()

	if hello := readFrame(t, c); hello["type"] != "hello" {
		t.Fatalf("want hello, got %v", hello)
	}
	env := readFrame(t, c)
	ev, _ := env["event"].(map[string]any)
	if env["type"] != "event" || ev["content"] != "hi" {
		t.Fatalf("want replayed assistant_text, got %v", env)
	}
}
