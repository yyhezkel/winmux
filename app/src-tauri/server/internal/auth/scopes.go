package auth

// scopes.go — per-device authorization grants (Phase 77 §Q6). A paired device
// carries a set of scopes (stored as a JSON array string on the device row). A
// missing/empty value or the sentinel "all" means "every grant" — the default
// on redeem, so existing devices keep full access (backwards compat) until an
// owner narrows them.

import (
	"encoding/json"
	"strings"
)

// Scope is a single capability grant.
type Scope string

const (
	ScopeWorkspaceRead  Scope = "workspace:read"
	ScopeWorkspaceWrite Scope = "workspace:write"
	ScopeSessionRead    Scope = "session:read"
	ScopeSessionWrite   Scope = "session:write"
	ScopeHookApprove    Scope = "hook:approve"
	ScopeHookDeny       Scope = "hook:deny"
	ScopeFilesRead      Scope = "files:read"
	ScopeFilesWrite     Scope = "files:write"
	ScopeInsightsRead   Scope = "insights:read"
)

// AllScopes is every known grant — also what an unrestricted device holds.
var AllScopes = []Scope{
	ScopeWorkspaceRead, ScopeWorkspaceWrite,
	ScopeSessionRead, ScopeSessionWrite,
	ScopeHookApprove, ScopeHookDeny,
	ScopeFilesRead, ScopeFilesWrite,
	ScopeInsightsRead,
}

// ValidScope reports whether s is a known scope.
func ValidScope(s string) bool {
	for _, k := range AllScopes {
		if Scope(s) == k {
			return true
		}
	}
	return false
}

// ParseScopes reads the stored scopes string into a grant list. "", "all", or
// a malformed value ⇒ AllScopes (fail-open for backwards compat — a device is
// only restricted by an explicit, well-formed subset).
func ParseScopes(stored string) []Scope {
	t := strings.TrimSpace(stored)
	if t == "" || t == "all" || t == `"all"` {
		return AllScopes
	}
	var arr []string
	if err := json.Unmarshal([]byte(t), &arr); err != nil {
		return AllScopes
	}
	out := make([]Scope, 0, len(arr))
	for _, x := range arr {
		out = append(out, Scope(x))
	}
	return out
}

// HasScope reports whether the stored scopes grant want.
func HasScope(stored string, want Scope) bool {
	for _, s := range ParseScopes(stored) {
		if s == want {
			return true
		}
	}
	return false
}

// NormalizeScopes validates + serializes a requested grant list to the stored
// JSON form. Unknown scopes are dropped. An empty/all-equivalent result stores
// "all". Returns the canonical stored string.
func NormalizeScopes(req []string) string {
	seen := map[string]bool{}
	var keep []string
	for _, s := range req {
		if ValidScope(s) && !seen[s] {
			seen[s] = true
			keep = append(keep, s)
		}
	}
	if len(keep) == 0 || len(keep) == len(AllScopes) {
		return "all"
	}
	b, _ := json.Marshal(keep)
	return string(b)
}
