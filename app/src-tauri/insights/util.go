package main

import (
	"crypto/rand"
	"encoding/hex"
)

// randHex returns n random bytes as a hex string (used for the API token).
func randHex(n int) string {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		// crypto/rand should never fail; fall back to a fixed-length zero
		// string rather than panicking the daemon at startup.
		return hex.EncodeToString(make([]byte, n))
	}
	return hex.EncodeToString(b)
}
