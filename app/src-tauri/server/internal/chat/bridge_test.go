package chat

import (
	"encoding/json"
	"path/filepath"
	"testing"

	"winmux-server/internal/workspace"
)

// TestBridgeTranslate locks the engine→workspace frame mapping (the shapes the
// mobile SDK consumes): the type renames + field renames (text→content,
// id→tool_id, name→tool_name, tool_use_id→tool_id). No Claude process needed —
// translate() only republishes into the workspace log.
func TestBridgeTranslate(t *testing.T) {
	st, err := workspace.OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	wm := workspace.NewManager(st, nil)
	ws, _ := wm.CreateWorkspace("p")
	se, _ := wm.CreateSession(ws.ID, KindClaudeChat)

	b := NewWorkspaceBridge(nil, wm) // mgr nil: translate never touches it
	b.translate(se.ID, []byte(`{"type":"assistant","text":"hi there"}`))
	b.translate(se.ID, []byte(`{"type":"tool_use","id":"t1","name":"Bash","input":{"cmd":"ls"}}`))
	b.translate(se.ID, []byte(`{"type":"tool_result","tool_use_id":"t1","content":"file.txt","is_error":false}`))
	b.translate(se.ID, []byte(`{"type":"status","status":"done"}`))
	b.translate(se.ID, []byte(`{"type":"session_init"}`)) // engine-internal → dropped

	evs, err := st.ReplayEvents(se.ID, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]map[string]any{}
	for _, ev := range evs {
		var p map[string]any
		_ = json.Unmarshal(ev.Payload, &p)
		got[ev.Type] = p
	}
	if got["assistant_text"]["content"] != "hi there" {
		t.Fatalf("assistant_text.content wrong: %v", got["assistant_text"])
	}
	if got["tool_use"]["tool_name"] != "Bash" || got["tool_use"]["tool_id"] != "t1" {
		t.Fatalf("tool_use renames wrong: %v", got["tool_use"])
	}
	if got["tool_result"]["tool_id"] != "t1" {
		t.Fatalf("tool_result.tool_id wrong: %v", got["tool_result"])
	}
	if got["status"]["status"] != "done" {
		t.Fatalf("status wrong: %v", got["status"])
	}
	if _, dropped := got["session_init"]; dropped {
		t.Fatal("session_init should be dropped, not published")
	}
}

// A non-claude_chat session gets no engine (nil), so a user_input is a safe
// no-op — the bridge must not panic or spawn.
func TestBridgeIgnoresNonClaudeKind(t *testing.T) {
	st, err := workspace.OpenStore(filepath.Join(t.TempDir(), "ws.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	wm := workspace.NewManager(st, nil)
	ws, _ := wm.CreateWorkspace("p")
	se, _ := wm.CreateSession(ws.ID, "plain") // not claude_chat
	b := NewWorkspaceBridge(nil, wm)
	b.OnUserInput(se.ID, "hello", "clientA") // must not spawn / panic
	if b.engine(se.ID) != nil {
		t.Fatal("no engine should be created for a non-claude_chat session")
	}
}
