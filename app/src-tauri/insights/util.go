package main

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"strings"
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

// tailFile returns the last `n` lines of a file. The insights log is size-
// capped (~1 MB) so reading it whole is fine; keeps the daemon dependency-free.
func tailFile(path string, n int) []string {
	data, err := os.ReadFile(path)
	if err != nil {
		return []string{}
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) == 1 && lines[0] == "" {
		return []string{}
	}
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return lines
}
