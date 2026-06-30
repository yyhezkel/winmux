package main

// Phase 69.C — hook bridge. The daemon implements the SERVER half of the
// Phase 66 RPC dialect so that Claude's hooks, fired inside a mobile-spawned
// session, reach the phone for approval with ZERO changes to the winmux CLI
// or hooks/claude-code.json.
//
// Flow: the daemon injects WINMUX_SOCKET_ADDR=<this listener>,
// WINMUX_TUNNEL_TOKEN=<per-session token>, WINMUX_PANE_ID=mob_<id> into the
// claude child (see spawnEnv). When Claude fires a PreToolUse hook, the CLI
// dials here, does the HMAC challenge-response, and pushes a
// permission_request. We map it to the session (by which session's token
// validates the HMAC), forward a hook_request over the WS, wait for the
// phone's allow/deny, and reply with the decision the CLI expects.
//
// Wire format (ported from cli/src/main.rs perform_handshake + rpc_via):
//   S->C  "WINMUX-CHALLENGE <nonce-hex>\n"
//   C->S  "WINMUX-RESPONSE <hmac_sha256(token, nonce_bytes)-hex>\n"
//   S->C  "WINMUX-OK\n"  |  "WINMUX-DENIED <reason>\n"
//   C->S  {"jsonrpc":"2.0","id":1,"method":"feed.push","params":{…}}\n
//   S->C  {"jsonrpc":"2.0","id":1,"result":{"request_id":…,"decision":…}}\n

import (
	"bufio"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"strings"
	"time"
)

func startHookRPC(m *SessionManager) {
	// Ephemeral localhost port — the daemon controls the address it injects
	// into each child, so we don't need a fixed port (and avoid a clash with
	// the metrics API). Listen synchronously so rpcAddr is set before any
	// session can spawn.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		log.Printf("chat: hook RPC listen failed, hooks won't reach mobile: %v", err)
		return
	}
	m.mu.Lock()
	m.rpcAddr = ln.Addr().String()
	m.mu.Unlock()
	log.Printf("chat: hook RPC listening on %s", ln.Addr().String())
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go m.handleHookConn(conn)
		}
	}()
}

func (m *SessionManager) handleHookConn(conn net.Conn) {
	defer conn.Close()
	br := bufio.NewReader(conn)

	// 1. Challenge.
	nonce := make([]byte, 16)
	if _, err := rand.Read(nonce); err != nil {
		return
	}
	_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
	if _, err := fmt.Fprintf(conn, "WINMUX-CHALLENGE %s\n", hex.EncodeToString(nonce)); err != nil {
		return
	}

	// 2. Response → identify the session by which token validates the HMAC.
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	respLine, err := br.ReadString('\n')
	if err != nil {
		return
	}
	respHex := strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(respLine), "WINMUX-RESPONSE "))
	respMAC, err := hex.DecodeString(respHex)
	if err != nil {
		_, _ = conn.Write([]byte("WINMUX-DENIED bad-response\n"))
		return
	}
	sess := m.matchSessionByHMAC(nonce, respMAC)
	if sess == nil {
		_, _ = conn.Write([]byte("WINMUX-DENIED unknown-session\n"))
		return
	}
	if _, err := conn.Write([]byte("WINMUX-OK\n")); err != nil {
		return
	}

	// 3. One JSON-RPC request.
	_ = conn.SetReadDeadline(time.Now().Add(15 * time.Second))
	reqLine, err := br.ReadString('\n')
	if err != nil {
		return
	}
	var req struct {
		ID     json.RawMessage `json:"id"`
		Method string          `json:"method"`
		Params json.RawMessage `json:"params"`
	}
	if json.Unmarshal([]byte(strings.TrimSpace(reqLine)), &req) != nil {
		return
	}

	result := m.dispatchHook(sess, req.Method, req.Params)

	// 4. Reply. No write deadline cap here beyond the OS — a blocking gate may
	// legitimately hold for up to wait_timeout_seconds.
	id := req.ID
	if len(id) == 0 {
		id = json.RawMessage("1")
	}
	resp := map[string]any{"jsonrpc": "2.0", "id": id, "result": result}
	_ = conn.SetWriteDeadline(time.Time{})
	out := jsonEvent(resp)
	_, _ = conn.Write(append(out, '\n'))
}

// matchSessionByHMAC finds the session whose per-session token produces the
// given HMAC over the nonce. O(active sessions); constant-time compare.
func (m *SessionManager) matchSessionByHMAC(nonce, mac []byte) *Session {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, s := range m.sessions {
		h := hmac.New(sha256.New, []byte(s.rpcToken))
		h.Write(nonce)
		if hmac.Equal(h.Sum(nil), mac) {
			return s
		}
	}
	return nil
}

type feedPushParams struct {
	RequestID          string          `json:"request_id"`
	Kind               string          `json:"kind"`
	Subkind            string          `json:"subkind"`
	PaneID             string          `json:"pane_id"`
	Title              string          `json:"title"`
	Summary            string          `json:"summary"`
	Payload            json.RawMessage `json:"payload"`
	WaitTimeoutSeconds int             `json:"wait_timeout_seconds"`
}

// dispatchHook handles a feed.push and returns the JSON-RPC result object.
func (m *SessionManager) dispatchHook(sess *Session, method string, rawParams json.RawMessage) map[string]any {
	if method != "feed.push" {
		return map[string]any{"decision": "deny", "error": "unknown method"}
	}
	var p feedPushParams
	if json.Unmarshal(rawParams, &p) != nil {
		return map[string]any{"decision": "deny"}
	}
	// Defense in depth: the pane_id must match the HMAC-identified session.
	if p.PaneID != "" && p.PaneID != "mob_"+sess.id {
		log.Printf("chat: hook pane_id %q != session mob_%s — denying", p.PaneID, sess.id)
		return map[string]any{"request_id": p.RequestID, "decision": "deny"}
	}

	// Passive lifecycle hooks: surface as a notification, ack immediately.
	if p.Kind != "permission_request" {
		sess.emit(jsonEvent(map[string]any{
			"type": "notification", "subkind": p.Subkind,
			"title": p.Title, "summary": p.Summary,
		}))
		return map[string]any{"request_id": p.RequestID, "decision": "passive"}
	}

	// Blocking permission request — apply the session policy.
	switch sess.policy {
	case "auto":
		return map[string]any{"request_id": p.RequestID, "decision": "allow"}
	case "block":
		return map[string]any{"request_id": p.RequestID, "decision": "deny"}
	}

	// gate (default): ask the phone.
	decision := sess.awaitHookDecision(&p, m.cfg.hookDenyOnTimeout)
	return map[string]any{"request_id": p.RequestID, "decision": decision}
}

// awaitHookDecision forwards a hook_request to connected clients and blocks
// until one answers, the timeout fires, or the session dies. Returns
// "allow" / "deny". Deny is the safe default on mobile (docs §9-Q5).
func (s *Session) awaitHookDecision(p *feedPushParams, denyOnTimeout bool) string {
	// No phone attached → nobody can approve; deny fast instead of stalling.
	if !s.hasSubscribers() {
		return "deny"
	}

	ch := make(chan string, 1)
	s.hookMu.Lock()
	s.pendingHooks[p.RequestID] = ch
	s.hookMu.Unlock()
	defer func() {
		s.hookMu.Lock()
		delete(s.pendingHooks, p.RequestID)
		s.hookMu.Unlock()
	}()

	toolName, toolInput := extractToolFields(p.Payload)
	prev := s.getStatus()
	s.setStatus(stWaitingHook)
	s.emit(jsonEvent(map[string]any{
		"type":              "hook_request",
		"req_id":            p.RequestID,
		"subkind":           p.Subkind,
		"tool_name":         toolName,
		"tool_input":        toolInput,
		"title":             p.Title,
		"decision_required": true,
	}))

	timeout := time.Duration(clampTimeout(p.WaitTimeoutSeconds)) * time.Second
	var decision, reason string
	select {
	case d := <-ch:
		decision, reason = d, "client"
	case <-time.After(timeout):
		reason = "timeout"
		if denyOnTimeout {
			decision = "deny"
		} else {
			decision = "allow"
		}
	}
	s.emit(jsonEvent(map[string]any{
		"type": "hook_resolved", "req_id": p.RequestID,
		"decision": decision, "reason": reason,
	}))
	if prev == stActive || prev == stWaitingInput {
		s.setStatus(prev)
	}
	return decision
}

func clampTimeout(s int) int {
	if s < 1 {
		return 120
	}
	if s > 600 {
		return 600
	}
	return s
}

// extractToolFields pulls tool_name / tool_input from a PreToolUse payload for
// a richer hook_request card. Best-effort.
func extractToolFields(payload json.RawMessage) (string, json.RawMessage) {
	if len(payload) == 0 {
		return "", json.RawMessage("null")
	}
	var p struct {
		ToolName  string          `json:"tool_name"`
		ToolInput json.RawMessage `json:"tool_input"`
	}
	if json.Unmarshal(payload, &p) != nil {
		return "", json.RawMessage("null")
	}
	if len(p.ToolInput) == 0 {
		p.ToolInput = json.RawMessage("null")
	}
	return p.ToolName, p.ToolInput
}
