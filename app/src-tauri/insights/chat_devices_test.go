package main

import (
	"errors"
	"fmt"
	"path/filepath"
	"testing"
)

func newTestChatAPI(t *testing.T) (*chatAPI, *SessionManager) {
	t.Helper()
	store, err := openChatStore(filepath.Join(t.TempDir(), "chat.db"))
	if err != nil {
		t.Fatalf("openChatStore: %v", err)
	}
	t.Cleanup(store.Close)
	mgr := newSessionManager(store)
	return newChatAPI(mgr, store, "shared-secret"), mgr
}

func TestDeviceTokenLifecycle(t *testing.T) {
	c, _ := newTestChatAPI(t)
	tok := randHex(32)
	dev := &DeviceRow{ID: "dev_1", TokenHash: hashToken(tok), Label: "phone", CreatedAt: nowUnix()}
	if err := c.store.insertDevice(dev); err != nil {
		t.Fatalf("insertDevice: %v", err)
	}
	got, ok := c.store.deviceByTokenHash(hashToken(tok))
	if !ok || got.ID != "dev_1" {
		t.Fatalf("lookup failed: %+v ok=%v", got, ok)
	}
	c.store.revokeDevice("dev_1")
	if _, ok := c.store.deviceByTokenHash(hashToken(tok)); ok {
		t.Fatal("revoked device still resolves")
	}
}

func TestAuthRoles(t *testing.T) {
	c, _ := newTestChatAPI(t)
	tok := randHex(32)
	_ = c.store.insertDevice(&DeviceRow{ID: "dev_2", TokenHash: hashToken(tok), CreatedAt: nowUnix()})

	if _, admin, ok := c.authDevice("shared-secret"); !ok || !admin {
		t.Fatal("shared token should be admin")
	}
	if dev, admin, ok := c.authDevice(tok); !ok || admin || dev != "dev_2" {
		t.Fatalf("device token: dev=%q admin=%v ok=%v", dev, admin, ok)
	}
	if _, _, ok := c.authDevice("garbage"); ok {
		t.Fatal("garbage token must not authenticate")
	}
}

func TestPerDeviceRateLimit(t *testing.T) {
	c, mgr := newTestChatAPI(t)
	mgr.cfg.maxPerDevice = 2
	for i := 0; i < 2; i++ {
		_ = c.store.insertSession(&SessionRow{
			ID: fmt.Sprintf("s%d", i), DeviceID: "dev_3", Status: stActive,
		})
	}
	_, err := mgr.create(startSpec{DeviceID: "dev_3"})
	var ce *chatErr
	if !errors.As(err, &ce) || ce.kind != "rate" {
		t.Fatalf("want rate error, got %v", err)
	}
}

func TestGlobalSessionCap(t *testing.T) {
	_, mgr := newTestChatAPI(t)
	mgr.cfg.maxGlobal = 0
	if _, err := mgr.create(startSpec{}); err == nil {
		t.Fatal("global cap of 0 should reject create")
	}
}

func TestSessionScopingByDevice(t *testing.T) {
	c, _ := newTestChatAPI(t)
	_ = c.store.insertSession(&SessionRow{ID: "a", DeviceID: "dev_A", Status: stActive})
	_ = c.store.insertSession(&SessionRow{ID: "b", DeviceID: "dev_B", Status: stActive})
	rowsA, _ := c.store.listSessionsForDevice("dev_A")
	if len(rowsA) != 1 || rowsA[0].ID != "a" {
		t.Fatalf("device A should see only its session, got %+v", rowsA)
	}
	all, _ := c.store.listSessions()
	if len(all) != 2 {
		t.Fatalf("admin listSessions should see all, got %d", len(all))
	}
	rowB, _ := c.store.getSession("b")
	if ownsSession("dev_A", rowB) {
		t.Fatal("dev_A must not own dev_B's session")
	}
	if !ownsSession("", rowB) {
		t.Fatal("admin must own any session")
	}
}
