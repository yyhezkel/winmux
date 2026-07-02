package chat

// bridge.go — Phase 77 §16: the engine↔substrate bridge. The Claude runner
// (Session, this package) was built for the retired /api/claude/* WS; the new
// workspace API (/api/v2/workspace/*) is a pure pub/sub substrate. This wires
// them: for a `claude_chat` workspace session the bridge lazily spawns a Claude
// process on the first user_input, feeds messages to its stdin, and republishes
// its output (assistant text, tool use/result, hooks, status) into the
// workspace event log — translating the engine's frame names to the §4.4 frame
// contract the mobile SDK consumes.
//
// It lives in `chat` (not a separate package) so it can drive the Session's
// unexported methods directly; it imports `workspace` (no cycle — workspace
// only imports core).

import (
	"encoding/json"
	"log"
	"sync"

	"winmux-server/internal/workspace"
)

// KindClaudeChat is the workspace session kind that gets a Claude engine.
const KindClaudeChat = "claude_chat"

// WorkspaceBridge connects Claude engine Sessions to workspace sessions.
type WorkspaceBridge struct {
	mgr   *SessionManager
	wsMgr *workspace.Manager
	mu    sync.Mutex
	live  map[string]*Session // wsSessionID → engine Session
}

// NewWorkspaceBridge builds the bridge. Register it with
// workspace.Manager.SetDriver so client input reaches it.
func NewWorkspaceBridge(mgr *SessionManager, wsMgr *workspace.Manager) *WorkspaceBridge {
	return &WorkspaceBridge{mgr: mgr, wsMgr: wsMgr, live: map[string]*Session{}}
}

// OnUserInput spawns the engine on first message (claude_chat only), then feeds
// the text to Claude. Implements workspace.SessionDriver.
func (b *WorkspaceBridge) OnUserInput(wsSessionID, content, _ string) {
	sess := b.engineFor(wsSessionID)
	if sess == nil {
		return
	}
	if err := sess.sendUserInput(content); err != nil {
		b.pub(wsSessionID, workspace.FrameError, map[string]any{"message": err.Error()})
	}
}

// OnHookDecision forwards the winning hook answer to Claude (unblocks it).
func (b *WorkspaceBridge) OnHookDecision(wsSessionID, reqID, _, decision string) {
	if sess := b.engine(wsSessionID); sess != nil {
		sess.resolveHook(reqID, decision)
	}
}

// OnInterrupt sends SIGINT to the running Claude process.
func (b *WorkspaceBridge) OnInterrupt(wsSessionID, _ string) {
	if sess := b.engine(wsSessionID); sess != nil {
		sess.interrupt()
	}
}

func (b *WorkspaceBridge) engine(wsSessionID string) *Session {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.live[wsSessionID]
}

// engineFor returns the engine Session for a claude_chat workspace session,
// spawning it on first use. Returns nil if the session isn't claude_chat or the
// spawn fails (an error frame is published in that case).
func (b *WorkspaceBridge) engineFor(wsSessionID string) *Session {
	if s := b.engine(wsSessionID); s != nil {
		return s
	}
	row, err := b.wsMgr.GetSession(wsSessionID)
	if err != nil || row.Kind != KindClaudeChat {
		return nil
	}
	sess, err := b.mgr.create(startSpec{})
	if err != nil {
		b.pub(wsSessionID, workspace.FrameError, map[string]any{"message": "claude spawn failed: " + err.Error()})
		return nil
	}
	b.mu.Lock()
	if existing, ok := b.live[wsSessionID]; ok { // lost a spawn race — discard ours
		b.mu.Unlock()
		sess.stop("duplicate engine")
		return existing
	}
	b.live[wsSessionID] = sess
	b.mu.Unlock()

	go b.pumpOut(wsSessionID, sess)
	b.pub(wsSessionID, workspace.FrameStatus, map[string]any{"status": "active"})
	return sess
}

// pumpOut streams the engine's output frames into the workspace log until the
// engine ends, then marks the session done and forgets it.
func (b *WorkspaceBridge) pumpOut(wsSessionID string, sess *Session) {
	sub := sess.addSubscriber()
	defer sess.removeSubscriber(sub)
	for raw := range sub.ch {
		b.translate(wsSessionID, raw)
	}
	b.mu.Lock()
	if b.live[wsSessionID] == sess {
		delete(b.live, wsSessionID)
	}
	b.mu.Unlock()
	b.pub(wsSessionID, workspace.FrameStatus, map[string]any{"status": "done"})
}

// translate maps one engine frame to a workspace frame (type + field renames).
func (b *WorkspaceBridge) translate(wsSessionID string, raw []byte) {
	var f map[string]any
	if json.Unmarshal(raw, &f) != nil {
		return
	}
	switch f["type"] {
	case "assistant", "assistant_delta", "text":
		if txt, _ := f["text"].(string); txt != "" {
			b.pub(wsSessionID, workspace.FrameAssistantText, map[string]any{"content": txt})
		}
	case "tool_use":
		b.pub(wsSessionID, workspace.FrameToolUse, map[string]any{
			"tool_id": f["id"], "tool_name": f["name"], "tool_input": f["input"],
		})
	case "tool_result":
		b.pub(wsSessionID, workspace.FrameToolResult, map[string]any{
			"tool_id": f["tool_use_id"], "content": f["content"], "is_error": f["is_error"],
		})
	case "hook_request":
		// Keep req_id identical so the workspace pending + engine hook share it.
		// CreateHookRequest publishes the hook_request frame + records the pending
		// (8b winner-takes-all across clients).
		reqID, _ := f["req_id"].(string)
		delete(f, "type")
		payload, _ := json.Marshal(f)
		if reqID != "" {
			_ = b.wsMgr.CreateHookRequest(wsSessionID, reqID, payload, 0)
		}
	case "status":
		if st, _ := f["status"].(string); st != "" {
			b.pub(wsSessionID, workspace.FrameStatus, map[string]any{"status": st})
		}
	case "error":
		b.pub(wsSessionID, workspace.FrameError, map[string]any{"message": f["message"]})
	case "notification":
		b.pub(wsSessionID, workspace.FrameNotification, map[string]any{
			"subkind": f["subkind"], "title": f["title"], "summary": f["summary"],
		})
		// hook_resolved: the workspace broadcasts its own on ResolveHook — no dup.
		// session_init / result / raw / user / system: engine-internal — dropped.
	}
}

func (b *WorkspaceBridge) pub(wsSessionID, typ string, payload map[string]any) {
	data, _ := json.Marshal(payload)
	if _, err := b.wsMgr.Publish(wsSessionID, typ, data); err != nil {
		log.Printf("bridge: publish %s → %s: %v", typ, wsSessionID, err)
	}
}
