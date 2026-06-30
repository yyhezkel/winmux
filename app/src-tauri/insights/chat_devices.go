package main

// Phase 69.D — device token management. Admin-only (shared/desktop token):
// the desktop mints a per-device bearer during 67.C pairing and can revoke it
// later without disturbing the metrics Monitor or other devices. The plaintext
// token is returned exactly once at creation; only its sha256 is stored
// (Rule #2).

import (
	"encoding/json"
	"net/http"
	"strings"
	"time"
)

func (c *chatAPI) registerDeviceRoutes(mux *http.ServeMux) {
	mux.HandleFunc("/api/claude/devices", c.adminGuard(c.handleDevices))
	mux.HandleFunc("/api/claude/devices/", c.adminGuard(c.handleDeviceItem))
}

func (c *chatAPI) handleDevices(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodPost:
		var body struct {
			Label string `json:"label"`
		}
		if r.Body != nil {
			_ = json.NewDecoder(r.Body).Decode(&body)
		}
		now := nowUnix()
		token := randHex(32)
		dev := &DeviceRow{
			ID:        "dev_" + randHex(6),
			TokenHash: hashToken(token),
			Label:     body.Label,
			CreatedAt: now,
		}
		if err := c.store.insertDevice(dev); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		// token returned ONCE; never retrievable again.
		writeJSON(w, map[string]any{
			"device_id": dev.ID,
			"label":     dev.Label,
			"token":     token,
		})
	case http.MethodGet:
		devs, err := c.store.listDevices()
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		out := make([]map[string]any, 0, len(devs))
		for _, d := range devs {
			out = append(out, map[string]any{
				"device_id":  d.ID,
				"label":      d.Label,
				"created_at": d.CreatedAt,
				"revoked":    d.RevokedAt != 0,
			})
		}
		writeJSON(w, map[string]any{"devices": out})
	default:
		http.Error(w, "GET or POST", http.StatusMethodNotAllowed)
	}
}

func (c *chatAPI) handleDeviceItem(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodDelete {
		http.Error(w, "DELETE only", http.StatusMethodNotAllowed)
		return
	}
	id := strings.Trim(strings.TrimPrefix(r.URL.Path, "/api/claude/devices/"), "/")
	if id == "" {
		http.Error(w, "missing device id", http.StatusBadRequest)
		return
	}
	c.store.revokeDevice(id)
	writeJSON(w, map[string]any{"ok": true})
}

func nowUnix() int64 { return time.Now().Unix() }
