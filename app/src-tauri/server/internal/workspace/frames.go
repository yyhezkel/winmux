package workspace

// frames.go — the WebSocket frame contract (Phase 77 S4.3), formalized as typed
// Go values so the producers can't drift from the published schema
// (docs/winmux-server/frames.schema.json + asyncapi.json).
//
// Design (locked S4.3 — no client is locked yet, so this is canonical):
//   - Discriminator is "type" (idiomatic; kotlinx @JsonClassDiscriminator, TS
//     tagged unions, AsyncAPI/JSON-Schema discriminator all default to it).
//   - Frames are FLAT: envelope fields (seq/session_id/ts) sit alongside the
//     type-specific fields — the natural target for a kotlinx sealed class with
//     a common base and for a TS discriminated union.
//   - snake_case everywhere, matching the REST surface (session_id, req_id,
//     frame_version, client_id, tool_name, is_error, resolved_by).
//   - Three families: control (hello), server→client session events, and
//     client→server commands.

import "winmux-server/internal/core"

// Frame type discriminators.
const (
	// Control.
	FrameHello = "hello"

	// Client→server commands.
	FrameUserInput    = "user_input"
	FrameHookDecision = "hook_decision"
	FrameInterrupt    = "interrupt"
	FrameUnsubscribe  = "unsubscribe"

	// Server→client session events emitted by the workspace substrate.
	FrameHookRequest  = "hook_request"
	FrameHookResolved = "hook_resolved"

	// Server→client content events emitted once a Claude session is attached to
	// the workspace (engine wiring, design §16). Named here so the schema + SDKs
	// carry them from day one.
	FrameAssistantText = "assistant_text"
	FrameToolUse       = "tool_use"
	FrameToolResult    = "tool_result"
	FrameStatus        = "status"
	FrameError         = "error"
	FrameNotification  = "notification"
)

// HelloFrame is the first server→client frame: stream capability negotiation.
// It carries no seq/ts (it is not a logged session event).
type HelloFrame struct {
	Type         string `json:"type"` // always FrameHello
	FrameVersion int    `json:"frame_version"`
	SessionID    string `json:"session_id"`
	ClientID     string `json:"client_id"`
}

func newHello(sessionID, clientID string) HelloFrame {
	return HelloFrame{
		Type:         FrameHello,
		FrameVersion: core.FrameVersion,
		SessionID:    sessionID,
		ClientID:     clientID,
	}
}

// hookResolvedPayload is the flat body merged into a hook_resolved frame.
type hookResolvedPayload struct {
	ReqID      string `json:"req_id"`
	Decision   string `json:"decision"` // "allow" | "deny"
	ResolvedBy string `json:"resolved_by"`
}

// userInputPayload is the flat body of a user_input frame (echoed to all
// subscribers so every attached client sees the same transcript).
type userInputPayload struct {
	Content  string `json:"content"`
	ClientID string `json:"client_id"`
}

// interruptPayload is the flat body of an interrupt frame.
type interruptPayload struct {
	ClientID string `json:"client_id"`
}

// clientFrame is the client→server command shape. A single struct covers all
// commands; unused fields stay zero for a given type.
type clientFrame struct {
	Type     string `json:"type"`
	Content  string `json:"content"`  // user_input
	ReqID    string `json:"req_id"`   // hook_decision
	Decision string `json:"decision"` // hook_decision: allow|deny
}
