package main

import (
	"encoding/json"
	"path/filepath"
	"testing"
)

// newTestSession builds a Session wired to a real (temp-file) chat store but
// with no spawned process, so the parser can be exercised in isolation.
func newTestSession(t *testing.T) (*Session, *subscriber) {
	t.Helper()
	store, err := openChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("openChatStore: %v", err)
	}
	t.Cleanup(store.Close)
	mgr := newSessionManager(store)
	s := &Session{
		id:           "mob_test",
		mgr:          mgr,
		status:       stActive,
		subs:         map[int64]*subscriber{},
		pendingHooks: map[string]chan string{},
	}
	_ = store.insertSession(&SessionRow{ID: s.id, Status: stActive, Policy: "gate"})
	sub := s.addSubscriber()
	return s, sub
}

// drain collects all events currently queued on the subscriber.
func drain(sub *subscriber) []map[string]any {
	var out []map[string]any
	for {
		select {
		case ev := <-sub.ch:
			var m map[string]any
			_ = json.Unmarshal(ev, &m)
			out = append(out, m)
		default:
			return out
		}
	}
}

func TestParserSystemInit(t *testing.T) {
	s, sub := newTestSession(t)
	s.handleClaudeLine([]byte(`{"type":"system","subtype":"init","session_id":"abc123","model":"claude-opus-4-8"}`))
	evs := drain(sub)
	if len(evs) != 1 || evs[0]["type"] != "session_init" {
		t.Fatalf("want session_init, got %+v", evs)
	}
	if evs[0]["claude_session_id"] != "abc123" {
		t.Fatalf("claude_session_id not captured: %+v", evs[0])
	}
	s.mu.Lock()
	got := s.claudeSessionID
	s.mu.Unlock()
	if got != "abc123" {
		t.Fatalf("session claudeSessionID = %q", got)
	}
}

func TestParserAssistantTextAndToolUse(t *testing.T) {
	s, sub := newTestSession(t)
	line := `{"type":"assistant","message":{"role":"assistant","content":[` +
		`{"type":"text","text":"Let me check."},` +
		`{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}`
	s.handleClaudeLine([]byte(line))
	evs := drain(sub)
	if len(evs) != 2 {
		t.Fatalf("want 2 events, got %d: %+v", len(evs), evs)
	}
	if evs[0]["type"] != "assistant" || evs[0]["text"] != "Let me check." {
		t.Fatalf("bad assistant event: %+v", evs[0])
	}
	if evs[1]["type"] != "tool_use" || evs[1]["name"] != "Bash" || evs[1]["id"] != "toolu_1" {
		t.Fatalf("bad tool_use event: %+v", evs[1])
	}
	s.mu.Lock()
	pend := s.pendingTool
	s.mu.Unlock()
	if pend != "Bash" {
		t.Fatalf("pendingTool = %q, want Bash", pend)
	}
}

func TestParserToolResultClearsPending(t *testing.T) {
	s, sub := newTestSession(t)
	s.setPendingTool("Bash")
	line := `{"type":"user","message":{"role":"user","content":[` +
		`{"type":"tool_result","tool_use_id":"toolu_1","content":"file.txt","is_error":false}]}}`
	s.handleClaudeLine([]byte(line))
	evs := drain(sub)
	if len(evs) != 1 || evs[0]["type"] != "tool_result" || evs[0]["tool_use_id"] != "toolu_1" {
		t.Fatalf("bad tool_result event: %+v", evs)
	}
	s.mu.Lock()
	pend := s.pendingTool
	s.mu.Unlock()
	if pend != "" {
		t.Fatalf("pendingTool not cleared: %q", pend)
	}
}

func TestParserResultSetsWaitingInput(t *testing.T) {
	s, sub := newTestSession(t)
	s.handleClaudeLine([]byte(`{"type":"result","subtype":"success","is_error":false,"result":"done"}`))
	evs := drain(sub)
	if len(evs) != 1 || evs[0]["type"] != "result" {
		t.Fatalf("want result, got %+v", evs)
	}
	if s.getStatus() != stWaitingInput {
		t.Fatalf("status = %q, want waiting_input", s.getStatus())
	}
}

func TestParserUnknownTypeForwardedRaw(t *testing.T) {
	s, sub := newTestSession(t)
	s.handleClaudeLine([]byte(`{"type":"mystery","foo":42}`))
	evs := drain(sub)
	if len(evs) != 1 || evs[0]["type"] != "raw" {
		t.Fatalf("want raw passthrough, got %+v", evs)
	}
}

func TestParserMalformedLineSkipped(t *testing.T) {
	s, sub := newTestSession(t)
	s.handleClaudeLine([]byte(`not json at all`))
	if evs := drain(sub); len(evs) != 0 {
		t.Fatalf("malformed line should emit nothing, got %+v", evs)
	}
}

func TestReplayBufferRoundTrips(t *testing.T) {
	s, _ := newTestSession(t)
	s.handleClaudeLine([]byte(`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}`))
	got := s.mgr.store.getReplay(s.id)
	if len(got) != 1 {
		t.Fatalf("replay len = %d, want 1", len(got))
	}
	var m map[string]any
	_ = json.Unmarshal(got[0], &m)
	if m["text"] != "hi" {
		t.Fatalf("replay content wrong: %+v", m)
	}
}
