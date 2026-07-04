package chat

// chat_push.go — Phase 77 native push. Exported wrappers over the device store
// so the push package + main wiring reach per-device state + the pending queue
// without exposing store internals.

// ResolveToken maps a bearer to (deviceID, admin, ok). The shared/admin token
// returns admin=true with an empty deviceID; a paired-device token returns its
// own device id.
func (c *ChatAPI) ResolveToken(token string) (deviceID string, admin, ok bool) {
	return c.authDevice(token)
}

// ActiveDeviceIDs lists every active paired device — the push fan-out targets.
// Satisfies workspace.PushLister.
func (c *ChatAPI) ActiveDeviceIDs() []string { return c.store.activeDeviceIDs() }

// EnqueuePush appends an event to a device's queue and returns its monotonic
// per-device push_seq. capN caps the queue (drop-oldest).
func (c *ChatAPI) EnqueuePush(deviceID, eventJSON string, capN int) (int64, error) {
	return c.store.enqueuePending(deviceID, eventJSON, capN)
}

// PendingPush returns a device's queued events with push_seq > cursor.
func (c *ChatAPI) PendingPush(deviceID string, cursor int64) []PendingEvent {
	return c.store.pendingAfter(deviceID, cursor)
}

// AckPush drops a device's queued events with push_seq <= upto.
func (c *ChatAPI) AckPush(deviceID string, upto int64) { c.store.ackPending(deviceID, upto) }

// PrunePush drops queued events older than cutoff (TTL sweep).
func (c *ChatAPI) PrunePush(cutoff int64) { c.store.prunePendingOlderThan(cutoff) }
