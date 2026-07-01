package chat

// Phase 69.B — Claude stream-json parser. Each stdout line is one JSON object
// with a top-level `type`. We normalize the events the chat UI needs and
// forward anything unrecognized verbatim under {"type":"raw"} so the protocol
// can evolve without a daemon redeploy.
//
// Rule #1: the user's prompts and Claude's output flow over the WS to the
// phone (that IS the product) but are NEVER written to a log. On a parse
// failure we log a length/metadata marker only — never the line content.

import (
	"encoding/json"
	"log"
)

type claudeEnvelope struct {
	Type      string          `json:"type"`
	Subtype   string          `json:"subtype"`
	SessionID string          `json:"session_id"`
	Model     string          `json:"model"`
	Message   json.RawMessage `json:"message"`
	Event     json.RawMessage `json:"event"`
	Result    *string         `json:"result"`
	IsError   bool            `json:"is_error"`
	Usage     json.RawMessage `json:"usage"`
}

type claudeMessage struct {
	Role    string        `json:"role"`
	Model   string        `json:"model"`
	Content []claudeBlock `json:"content"`
}

type claudeBlock struct {
	Type string `json:"type"`
	// text
	Text string `json:"text"`
	// tool_use
	ID    string          `json:"id"`
	Name  string          `json:"name"`
	Input json.RawMessage `json:"input"`
	// tool_result
	ToolUseID string          `json:"tool_use_id"`
	Content   json.RawMessage `json:"content"` // string OR array of blocks
	IsError   bool            `json:"is_error"`
}

func (s *Session) handleClaudeLine(line []byte) {
	var env claudeEnvelope
	if err := json.Unmarshal(line, &env); err != nil {
		// Metadata only (Rule #1) — never the content.
		log.Printf("chat: session %s unparsable stdout line (%d bytes)", s.id, len(line))
		return
	}
	s.mgr.store.bumpActivity(s.id, 0)

	switch env.Type {
	case "system":
		if env.Subtype == "init" {
			if env.SessionID != "" {
				s.mu.Lock()
				s.claudeSessionID = env.SessionID
				s.mu.Unlock()
				s.mgr.store.setClaudeSessionID(s.id, env.SessionID)
			}
			s.emit(jsonEvent(map[string]any{
				"type":              "session_init",
				"claude_session_id": env.SessionID,
				"model":             env.Model,
			}))
		}

	case "assistant":
		s.setStatus(stActive)
		s.emitMessageBlocks(env.Message, true)

	case "user":
		// Tool results come back wrapped as a user message.
		s.emitMessageBlocks(env.Message, false)

	case "stream_event":
		// Partial token streaming (only when --include-partial-messages).
		if text := extractDeltaText(env.Event); text != "" {
			s.emit(jsonEvent(map[string]any{"type": "assistant_delta", "text": text}))
		}

	case "result":
		s.setStatus(stWaitingInput)
		ev := map[string]any{
			"type":     "result",
			"subtype":  env.Subtype,
			"is_error": env.IsError,
		}
		if env.Result != nil {
			ev["result"] = *env.Result
		}
		if len(env.Usage) > 0 {
			ev["usage"] = env.Usage
		}
		s.emit(jsonEvent(ev))

	default:
		// Unknown event — forward verbatim so the phone (and we) can evolve.
		s.emit(jsonEvent(map[string]any{"type": "raw", "event": json.RawMessage(line)}))
	}
}

// emitMessageBlocks walks a message's content blocks and emits one normalized
// WS event per block. `assistant` distinguishes assistant turns (text +
// tool_use) from user turns (tool_result).
func (s *Session) emitMessageBlocks(raw json.RawMessage, assistant bool) {
	if len(raw) == 0 {
		return
	}
	var msg claudeMessage
	if err := json.Unmarshal(raw, &msg); err != nil {
		log.Printf("chat: session %s message block parse failed", s.id)
		return
	}
	for _, b := range msg.Content {
		switch b.Type {
		case "text":
			if b.Text != "" {
				s.emit(jsonEvent(map[string]any{
					"type": "assistant", "text": b.Text, "partial": false,
				}))
			}
		case "tool_use":
			s.setPendingTool(b.Name)
			s.emit(jsonEvent(map[string]any{
				"type": "tool_use", "id": b.ID, "name": b.Name,
				"input": rawOrNull(b.Input),
			}))
		case "tool_result":
			s.setPendingTool("")
			s.emit(jsonEvent(map[string]any{
				"type": "tool_result", "tool_use_id": b.ToolUseID,
				"content": rawOrNull(b.Content), "is_error": b.IsError,
			}))
		default:
			if assistant {
				s.emit(jsonEvent(map[string]any{
					"type": "raw", "event": rawOrNull(raw),
				}))
			}
		}
	}
}

// extractDeltaText pulls text out of an Anthropic streaming delta event
// (content_block_delta → delta.text). Returns "" for non-text deltas.
func extractDeltaText(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	var ev struct {
		Type  string `json:"type"`
		Delta struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"delta"`
	}
	if json.Unmarshal(raw, &ev) != nil {
		return ""
	}
	if ev.Type == "content_block_delta" && ev.Delta.Type == "text_delta" {
		return ev.Delta.Text
	}
	return ""
}

func rawOrNull(r json.RawMessage) json.RawMessage {
	if len(r) == 0 {
		return json.RawMessage("null")
	}
	return r
}

// setPendingTool records the in-flight tool name (for the session summary).
func (s *Session) setPendingTool(name string) {
	s.mu.Lock()
	s.pendingTool = name
	s.mu.Unlock()
}
