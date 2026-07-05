package chat

import (
	"path/filepath"
	"testing"
)

func TestChatAPIScopes(t *testing.T) {
	store, err := OpenChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("OpenChatStore: %v", err)
	}
	defer store.Close()
	c := NewChatAPI(NewSessionManager(store), store, "SHARED")

	// An active paired device with a known long-term token.
	tok := "device-long-term-token"
	if _, err := store.db.Exec(
		`INSERT INTO paired_devices
		   (device_id, device_name, token_hash, ots_hash, scopes, status,
		    created_at, expires_at, last_seen, last_ip)
		 VALUES('dev_x','phone',?, '', 'all', 'active', 0, 0, 0, '')`,
		hashToken(tok)); err != nil {
		t.Fatalf("insert device: %v", err)
	}

	// Device token → its scopes; shared token → admin bypass.
	scopes, admin, ok := c.DeviceScopes(tok)
	if !ok || admin || scopes != "all" {
		t.Fatalf("DeviceScopes(device) = %q admin=%v ok=%v", scopes, admin, ok)
	}
	if _, adm, ok := c.DeviceScopes("SHARED"); !ok || !adm {
		t.Fatal("shared token should resolve as admin")
	}
	if _, _, ok := c.DeviceScopes("bogus"); ok {
		t.Fatal("unknown token must not resolve")
	}

	// Narrow grants, read back.
	if !c.SetDeviceScopes("dev_x", `["workspace:read"]`) {
		t.Fatal("SetDeviceScopes failed")
	}
	if s, ok := c.GetDeviceScopes("dev_x"); !ok || s != `["workspace:read"]` {
		t.Fatalf("GetDeviceScopes = %q ok=%v", s, ok)
	}
	// Setting scopes on an unknown device fails.
	if c.SetDeviceScopes("dev_missing", "all") {
		t.Fatal("SetDeviceScopes on unknown device should fail")
	}
}
