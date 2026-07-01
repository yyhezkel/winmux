package chat

// Phase 70.B — mobile pairing endpoints. The desktop (admin token) ISSUES a
// pending device with a one-shot token (shown as a QR). The phone REDEEMS the
// one-shot for a long-term bearer, which then authenticates all REST/WS calls
// (Insights + chat) via the existing auth, now backed by paired_devices.
//
// Tokens are returned in plaintext exactly once; only sha256 hashes persist
// (Rule #2). One-shots are single-use + short-TTL.

import (
	"encoding/json"
	"net/http"
	"strings"
	"time"
)

// pairingTTL — how long a one-shot pairing token is valid.
const pairingTTL = 5 * time.Minute

func nowUnix() int64 { return time.Now().Unix() }

func (c *ChatAPI) registerPairingRoutes(mux *http.ServeMux) {
	mux.HandleFunc("/api/pairing/issue", c.adminGuard(c.handlePairIssue))
	mux.HandleFunc("/api/pairing/redeem", c.handlePairRedeem) // auth = the one-shot itself
	mux.HandleFunc("/api/pairing/devices", c.adminGuard(c.handlePairDevices))
	mux.HandleFunc("/api/pairing/devices/", c.adminGuard(c.handlePairDeviceItem))
}

// POST /api/pairing/issue (admin) → { device_id, one_shot_token, expires_at }
func (c *ChatAPI) handlePairIssue(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "POST only", http.StatusMethodNotAllowed)
		return
	}
	var body struct {
		DeviceName string   `json:"device_name"`
		Scopes     []string `json:"scopes"`
	}
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&body)
	}
	scopes := "all" // decision #4: all scopes always
	if len(body.Scopes) > 0 {
		if b, err := json.Marshal(body.Scopes); err == nil {
			scopes = string(b)
		}
	}
	now := nowUnix()
	oneShot := randHex(24)
	dev := &PairedDevice{
		ID:        "dev_" + randHex(6),
		Name:      body.DeviceName,
		OtsHash:   hashToken(oneShot),
		Scopes:    scopes,
		CreatedAt: now,
		ExpiresAt: now + int64(pairingTTL.Seconds()),
	}
	if err := c.store.issueDevice(dev); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{
		"device_id":      dev.ID,
		"one_shot_token": oneShot, // shown once (becomes the QR)
		"expires_at":     dev.ExpiresAt,
	})
}

// POST /api/pairing/redeem → { device_id, long_term_token }
// No bearer: the one-shot token in the body IS the credential.
func (c *ChatAPI) handlePairRedeem(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "POST only", http.StatusMethodNotAllowed)
		return
	}
	var body struct {
		OneShotToken string `json:"one_shot_token"`
	}
	if r.Body != nil {
		_ = json.NewDecoder(r.Body).Decode(&body)
	}
	if body.OneShotToken == "" {
		http.Error(w, "missing one_shot_token", http.StatusBadRequest)
		return
	}
	longTerm := randHex(32)
	id, ok := c.store.redeemDevice(hashToken(body.OneShotToken), hashToken(longTerm), nowUnix())
	if !ok {
		http.Error(w, "invalid or expired pairing token", http.StatusUnauthorized)
		return
	}
	c.store.touchDevice(id, clientIP(r))
	writeJSON(w, map[string]any{
		"device_id":       id,
		"long_term_token": longTerm, // shown once; the phone stores it
	})
}

// GET /api/pairing/devices (admin) → { devices: [...] }
func (c *ChatAPI) handlePairDevices(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "GET only", http.StatusMethodNotAllowed)
		return
	}
	devs, err := c.store.listDevices()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	out := make([]map[string]any, 0, len(devs))
	for _, d := range devs {
		out = append(out, map[string]any{
			"device_id":   d.ID,
			"device_name": d.Name,
			"status":      d.Status,
			"scopes":      d.Scopes,
			"created_at":  d.CreatedAt,
			"expires_at":  d.ExpiresAt,
			"last_seen":   d.LastSeen,
			"last_ip":     d.LastIP,
		})
	}
	writeJSON(w, map[string]any{"devices": out})
}

// DELETE /api/pairing/devices/{id}            (revoke)
// PUT    /api/pairing/devices/{id}/name       (rename, body {name})
func (c *ChatAPI) handlePairDeviceItem(w http.ResponseWriter, r *http.Request) {
	rest := strings.Trim(strings.TrimPrefix(r.URL.Path, "/api/pairing/devices/"), "/")
	parts := strings.Split(rest, "/")
	id := parts[0]
	if id == "" {
		http.Error(w, "missing device id", http.StatusBadRequest)
		return
	}
	switch {
	case r.Method == http.MethodDelete && len(parts) == 1:
		c.store.revokeDevice(id)
		writeJSON(w, map[string]any{"ok": true})
	case r.Method == http.MethodPut && len(parts) == 2 && parts[1] == "name":
		var body struct {
			Name string `json:"name"`
		}
		if r.Body != nil {
			_ = json.NewDecoder(r.Body).Decode(&body)
		}
		c.store.renameDevice(id, body.Name)
		writeJSON(w, map[string]any{"ok": true})
	default:
		http.Error(w, "DELETE {id} or PUT {id}/name", http.StatusMethodNotAllowed)
	}
}
