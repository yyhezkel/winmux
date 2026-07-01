package workspace

// ws.go — the session subscribe WebSocket (8a/8b). Handshake: the client opens
// GET /api/v2/workspace/{id}/session/{sid}/subscribe?cursor=N[&client_id=&device_name=]
// with the bearer token (Authorization header OR ?token= for clients that can't
// set headers). The server sends a `hello` (frame_version), replays every event
// after the cursor, then streams live frames; concurrently it reads client→
// server frames (user_input / hook_decision / interrupt / unsubscribe).

import (
	"encoding/json"
	"net/http"
	"strconv"
	"strings"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"

	"winmux-server/internal/core"
)

var upgrader = websocket.Upgrader{
	// The port is bearer-gated and localhost/tunnel-only; origin is not a
	// meaningful check here.
	CheckOrigin: func(*http.Request) bool { return true },
}

// wsAuthorized checks the bearer token from the header or the ?token= query.
func (s *Service) wsAuthorized(r *http.Request) bool {
	if s.token == "" {
		return true // token unset (tests) → open
	}
	got := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
	if got == s.token {
		return true
	}
	return r.URL.Query().Get("token") == s.token
}

func (s *Service) handleSubscribe(w http.ResponseWriter, r *http.Request) {
	if !s.wsAuthorized(r) {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	sid := r.PathValue("sid")
	if _, err := s.mgr.GetSession(sid); err != nil {
		http.Error(w, "unknown session", http.StatusNotFound)
		return
	}
	clientID := r.URL.Query().Get("client_id")
	if clientID == "" {
		clientID = "anon_" + uuid.NewString()[:8]
	}
	deviceName := r.URL.Query().Get("device_name")
	var cursor int64
	if v := r.URL.Query().Get("cursor"); v != "" {
		cursor, _ = strconv.ParseInt(v, 10, 64)
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()

	replay, ch, cancel, err := s.mgr.Subscribe(sid, clientID, deviceName, cursor)
	if err != nil {
		return
	}
	defer cancel()

	// hello → capability negotiation for the stream (§4.4).
	hello, _ := json.Marshal(map[string]any{
		"type": "hello", "frame_version": core.FrameVersion, "session_id": sid, "client_id": clientID,
	})
	if conn.WriteMessage(websocket.TextMessage, hello) != nil {
		return
	}

	// Replay everything after the cursor; track the high-water seq so the live
	// stream can skip any event that overlaps the replay window.
	lastSeq := cursor
	for _, ev := range replay {
		if conn.WriteMessage(websocket.TextMessage, eventFrame(ev)) != nil {
			return
		}
		lastSeq = ev.Seq
	}

	// Read client→server frames concurrently; the only writer is this goroutine.
	go s.readLoop(conn, sid, clientID)

	for frame := range ch {
		var probe struct {
			Seq int64 `json:"seq"`
		}
		_ = json.Unmarshal(frame, &probe)
		if probe.Seq != 0 && probe.Seq <= lastSeq {
			continue // already delivered in the replay window
		}
		if conn.WriteMessage(websocket.TextMessage, frame) != nil {
			return
		}
		if probe.Seq > lastSeq {
			lastSeq = probe.Seq
		}
	}
}

// readLoop processes inbound frames. It never writes to the socket (the write
// side is the subscribe goroutine); its actions surface as events fanned back
// out through the shared log.
func (s *Service) readLoop(conn *websocket.Conn, sid, clientID string) {
	for {
		_, data, err := conn.ReadMessage()
		if err != nil {
			return
		}
		var f struct {
			Type     string `json:"type"`
			Content  string `json:"content"`
			ReqID    string `json:"req_id"`
			Decision string `json:"decision"`
		}
		if json.Unmarshal(data, &f) != nil {
			continue
		}
		switch f.Type {
		case "user_input":
			payload, _ := json.Marshal(map[string]any{"content": f.Content, "client_id": clientID})
			_, _ = s.mgr.Publish(sid, "user_input", payload)
		case "hook_decision":
			_, _ = s.mgr.ResolveHook(f.ReqID, clientID, f.Decision)
		case "interrupt":
			payload, _ := json.Marshal(map[string]any{"client_id": clientID})
			_, _ = s.mgr.Publish(sid, "interrupt", payload)
		case "unsubscribe":
			_ = conn.Close() // ends the write loop too
			return
		}
	}
}
