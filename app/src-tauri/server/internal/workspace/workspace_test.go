package workspace

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

func newMgr(t *testing.T) *Manager {
	t.Helper()
	st, err := OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.Close)
	return NewManager(st, nil)
}

func drainForType(ch <-chan []byte, typ string) map[string]any {
	timeout := time.After(2 * time.Second)
	for {
		select {
		case f := <-ch:
			var m map[string]any
			_ = json.Unmarshal(f, &m)
			if m["type"] == typ {
				return m
			}
		case <-timeout:
			return nil
		}
	}
}

func TestWorkspaceCRUDCascade(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("proj")
	se, err := m.CreateSession(w.ID, KindClaudeChat)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.Publish(se.ID, "assistant_text", json.RawMessage(`{"content":"hi"}`)); err != nil {
		t.Fatal(err)
	}
	if n := m.store.EventCount(se.ID); n != 1 {
		t.Fatalf("event count = %d", n)
	}
	// creating a session under a missing workspace is rejected
	if _, err := m.CreateSession("ws_nope", ""); err != ErrNotFound {
		t.Fatalf("want ErrNotFound, got %v", err)
	}
	// delete cascades sessions + events
	if err := m.DeleteWorkspace(w.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := m.GetWorkspace(w.ID); err != ErrNotFound {
		t.Fatal("workspace should be gone")
	}
	if _, err := m.GetSession(se.ID); err != ErrNotFound {
		t.Fatal("session should be gone")
	}
	if n := m.store.EventCount(se.ID); n != 0 {
		t.Fatalf("events should be gone, got %d", n)
	}
}

func TestEventReplayFromCursor(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	for i := 0; i < 5; i++ {
		if _, err := m.Publish(se.ID, "assistant_text", json.RawMessage(`{}`)); err != nil {
			t.Fatal(err)
		}
	}
	// replay from cursor 2 → seq 3,4,5
	rep, ch, cancel, err := m.Subscribe(se.ID, "clientA", "phone", 2)
	if err != nil {
		t.Fatal(err)
	}
	defer cancel()
	if len(rep) != 3 || rep[0].Seq != 3 || rep[2].Seq != 5 {
		t.Fatalf("replay wrong: %+v", rep)
	}
	// live: a new publish reaches the channel with seq 6
	if _, err := m.Publish(se.ID, "assistant_text", json.RawMessage(`{}`)); err != nil {
		t.Fatal(err)
	}
	f := drainForType(ch, "assistant_text")
	if f == nil || f["seq"].(float64) != 6 {
		t.Fatalf("live frame: %v", f)
	}
}

func TestTwoSubscribersBothReceive(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	_, chA, cancelA, _ := m.Subscribe(se.ID, "A", "pa", 0)
	defer cancelA()
	_, chB, cancelB, _ := m.Subscribe(se.ID, "B", "pb", 0)
	defer cancelB()
	if m.SubscriberCount(se.ID) != 2 {
		t.Fatalf("want 2 subscribers, got %d", m.SubscriberCount(se.ID))
	}
	if _, err := m.Publish(se.ID, "tool_use", json.RawMessage(`{"tool_name":"Bash"}`)); err != nil {
		t.Fatal(err)
	}
	for name, ch := range map[string]<-chan []byte{"A": chA, "B": chB} {
		if f := drainForType(ch, "tool_use"); f == nil || f["tool_name"] != "Bash" {
			t.Fatalf("subscriber %s missed the frame: %v", name, f)
		}
	}
}

func TestHookWinnerTakesAll(t *testing.T) {
	m := newMgr(t)
	w, _ := m.CreateWorkspace("p")
	se, _ := m.CreateSession(w.ID, "")
	_, ch, cancel, _ := m.Subscribe(se.ID, "obs", "o", 0)
	defer cancel()
	if err := m.CreateHookRequest(se.ID, "req1", json.RawMessage(`{"req_id":"req1","tool_name":"Bash"}`), 0); err != nil {
		t.Fatal(err)
	}

	// 10 clients race to decide; exactly one must win.
	var wg sync.WaitGroup
	wins := make([]bool, 10)
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			won, _ := m.ResolveHook("req1", fmt.Sprintf("client%d", i), "allow")
			wins[i] = won
		}(i)
	}
	wg.Wait()
	count := 0
	for _, ok := range wins {
		if ok {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("winner-takes-all: expected exactly 1 winner, got %d", count)
	}
	// all subscribers get a hook_resolved broadcast
	if f := drainForType(ch, "hook_resolved"); f == nil || f["decision"] != "allow" {
		t.Fatalf("expected a hook_resolved broadcast, got %v", f)
	}
}
