package chat

// chat_fcm.go — Phase 77 §7. Thin exported wrappers over the device store so
// the API layer (fcm-token endpoint) and the main wiring (FCM sender resolver +
// workspace push lister) can reach per-device push state without exporting the
// store internals.

// FCMTokenForDevice resolves an active device's Firebase registration token.
// Satisfies fcm.TokenResolver (deviceID → token, ok).
func (c *ChatAPI) FCMTokenForDevice(id string) (string, bool) {
	return c.store.fcmTokenForDevice(id)
}

// ActivePushDeviceIDs lists active devices with a registration token.
// Satisfies workspace.PushLister — the out-of-band push fan-out targets.
func (c *ChatAPI) ActivePushDeviceIDs() []string {
	return c.store.activePushDeviceIDs()
}

// SetDeviceFCMToken records (or clears) an active device's registration token.
// Returns false when the device is unknown or not active.
func (c *ChatAPI) SetDeviceFCMToken(deviceID, token string) bool {
	return c.store.setDeviceFCMToken(deviceID, token)
}

// ResolveToken maps a bearer to (deviceID, admin, ok). The shared/admin token
// returns admin=true with an empty deviceID; a paired-device token returns its
// device id. Used by the fcm-token endpoint to bind the token to its own device.
func (c *ChatAPI) ResolveToken(token string) (deviceID string, admin, ok bool) {
	return c.authDevice(token)
}
