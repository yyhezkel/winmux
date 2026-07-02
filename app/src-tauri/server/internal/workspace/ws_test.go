package workspace

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func testWSServer(t *testing.T) (*Manager, *httptest.Server) {
	t.Helper()
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	m := NewManager(st, nil)
	mux := http.NewServeMux()
	NewService(m, "").RegisterRoutes(mux, func(h http.HandlerFunc) http.HandlerFunc { return h })
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return m, srv
}

func dialSub(t *testing.T, srv *httptest.Server, sid, clientID string, cursor int) *websocket.Conn {
	t.Helper()
	u := "ws" + strings.TrimPrefix(srv.URL, "http") +
		"/api/v2/workspace/x/session/" + sid + "/subscribe?client_id=" + clientID + "&cursor=" + strconv.Itoa(cursor)
	c, _, err := websocket.DefaultDialer.Dial(u, nil)
	if err != nil {
		t.Fatalf("dial %s: %v", clientID, err)
	}
	if h := readFrame(t, c); h["type"] != "hello" { // handshake: hello first
		t.Fatalf("want hello, got %v", h)
	}
	return c
}

func readFrame(t *testing.T, c *websocket.Conn) map[string]any {
	t.Helper()
	_ = c.SetReadDeadline(time.Now().Add(3 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	var m map[string]any
	_ = json.Unmarshal(msg, &m)
	return m
}

func readType(t *testing.T, c *websocket.Conn, typ string) map[string]any {
	t.Helper()
	for i := 0; i < 20; i++ {
		if m := readFrame(t, c); m["type"] == typ {
			return m
		}
	}
	t.Fatalf("never saw a %q frame", typ)
	return nil
}

// 8a + 8b over the wire: two clients on one session both see events; one decides
// a hook, both see the resolution.
func TestWSTwoClientsAndHookResolve(t *testing.T) {
	m, srv := testWSServer(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")

	a := dialSub(t, srv, se.ID, "A", 0)
	defer a.Close()
	b := dialSub(t, srv, se.ID, "B", 0)
	defer b.Close()
	if m.SubscriberCount(se.ID) != 2 {
		t.Fatalf("want 2 subscribers, got %d", m.SubscriberCount(se.ID))
	}

	// 8a: a published event reaches BOTH clients.
	if _, err := m.Publish(se.ID, "assistant_text", json.RawMessage(`{"content":"hi"}`)); err != nil {
		t.Fatal(err)
	}
	if fa, fb := readType(t, a, "assistant_text"), readType(t, b, "assistant_text"); fa["content"] != "hi" || fb["content"] != "hi" {
		t.Fatalf("assistant_text not fanned out: a=%v b=%v", fa, fb)
	}

	// 8b: a hook request reaches both; A answers; both see hook_resolved.
	if err := m.CreateHookRequest(se.ID, "req1", json.RawMessage(`{"req_id":"req1","tool_name":"Bash"}`), 0); err != nil {
		t.Fatal(err)
	}
	readType(t, a, "hook_request")
	readType(t, b, "hook_request")
	if err := a.WriteMessage(websocket.TextMessage,
		[]byte(`{"type":"hook_decision","req_id":"req1","decision":"allow"}`)); err != nil {
		t.Fatal(err)
	}
	ra := readType(t, a, "hook_resolved")
	rb := readType(t, b, "hook_resolved")
	if ra["resolved_by"] != "A" || rb["decision"] != "allow" {
		t.Fatalf("hook_resolved wrong: a=%v b=%v", ra, rb)
	}
}

// The subscribe WS must accept a paired-device token (not only the shared
// token) — the bug that 401'd a phone after it created a session over REST.
func TestWSAcceptsDeviceToken(t *testing.T) {
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	m := NewManager(st, nil)
	svc := NewService(m, "shared-token")
	svc.SetDeviceAuth(func(tok string) bool { return tok == "device-token" || tok == "shared-token" })
	mux := http.NewServeMux()
	svc.RegisterRoutes(mux, func(h http.HandlerFunc) http.HandlerFunc { return h })
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)

	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	base := "ws" + strings.TrimPrefix(srv.URL, "http") + "/api/v2/workspace/x/session/" + se.ID + "/subscribe?client_id=P&token="

	// device token → connects (hello).
	c, _, err := websocket.DefaultDialer.Dial(base+"device-token", nil)
	if err != nil {
		t.Fatalf("device token should authorize the WS: %v", err)
	}
	if h := readFrame(t, c); h["type"] != "hello" {
		t.Fatalf("want hello, got %v", h)
	}
	c.Close()

	// a bogus token → 401 (handshake fails).
	if _, resp, err := websocket.DefaultDialer.Dial(base+"nope", nil); err == nil {
		t.Fatal("bogus token must be rejected")
	} else if resp == nil || resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("want 401, got %v", resp)
	}
}

// A late subscriber replays the whole history from its cursor, then streams.
func TestWSReconnectReplaysFromCursor(t *testing.T) {
	m, srv := testWSServer(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	for i := 0; i < 3; i++ {
		_, _ = m.Publish(se.ID, "assistant_text", json.RawMessage(`{}`))
	}
	c := dialSub(t, srv, se.ID, "late", 0)
	defer c.Close()
	for want := int64(1); want <= 3; want++ {
		if got := int64(readFrame(t, c)["seq"].(float64)); got != want {
			t.Fatalf("replay seq: want %d got %d", want, got)
		}
	}
	// then a live event streams as seq 4.
	_, _ = m.Publish(se.ID, "assistant_text", json.RawMessage(`{}`))
	if got := int64(readType(t, c, "assistant_text")["seq"].(float64)); got != 4 {
		t.Fatalf("live seq: want 4 got %d", got)
	}
}
