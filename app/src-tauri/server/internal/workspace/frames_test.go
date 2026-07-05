package workspace

import (
	"encoding/json"
	"testing"
)

// TestFrameWireShapes locks the on-the-wire frame shapes to the published
// contract (asyncapi.json / frames.schema.json): the `type` discriminator
// values, the required envelope keys (seq/session_id/ts), and the snake_case
// field names. If a producer drifts, this fails before the SDK diff-guard would.
func TestFrameWireShapes(t *testing.T) {
	decode := func(b []byte) map[string]any {
		var m map[string]any
		if err := json.Unmarshal(b, &m); err != nil {
			t.Fatalf("frame is not valid JSON: %v", err)
		}
		return m
	}
	hasKeys := func(name string, m map[string]any, keys ...string) {
		for _, k := range keys {
			if _, ok := m[k]; !ok {
				t.Errorf("%s frame missing key %q (got %v)", name, k, keys)
			}
		}
	}

	// hello — control frame, no seq/ts.
	hello := decode(mustJSON(t, newHello("sess_x", "A")))
	if hello["type"] != FrameHello {
		t.Errorf("hello type = %v, want %q", hello["type"], FrameHello)
	}
	hasKeys("hello", hello, "type", "frame_version", "session_id", "client_id")

	// Session events all carry the envelope (seq/session_id/ts) via eventFrame.
	ev := func(typ string, payload any) map[string]any {
		return decode(eventFrame(Event{
			Seq: 7, SessionID: "sess_x", Type: typ, Timestamp: 1782900000,
			Payload: mustJSON(t, payload),
		}))
	}

	hr := ev(FrameHookResolved, hookResolvedPayload{ReqID: "req1", Decision: "allow", ResolvedBy: "A"})
	if hr["type"] != FrameHookResolved || hr["decision"] != "allow" || hr["resolved_by"] != "A" {
		t.Errorf("hook_resolved wrong: %v", hr)
	}
	hasKeys("hook_resolved", hr, "type", "seq", "session_id", "ts", "req_id", "decision", "resolved_by")

	ui := ev(FrameUserInput, userInputPayload{Content: "hi", ClientID: "A"})
	hasKeys("user_input", ui, "type", "seq", "session_id", "ts", "content", "client_id")

	it := ev(FrameInterrupt, interruptPayload{ClientID: "A"})
	hasKeys("interrupt", it, "type", "seq", "session_id", "ts", "client_id")

	// hook_request carries whatever the producer publishes; envelope still merged.
	hq := ev(FrameHookRequest, map[string]any{"req_id": "req1", "tool_name": "Bash"})
	hasKeys("hook_request", hq, "type", "seq", "session_id", "ts", "req_id", "tool_name")
}

func mustJSON(t *testing.T, v any) []byte {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	return b
}
