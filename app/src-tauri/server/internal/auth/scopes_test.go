package auth

import "testing"

func TestParseScopesDefaultsToAll(t *testing.T) {
	for _, in := range []string{"", "all", `"all"`, "not-json"} {
		if got := ParseScopes(in); len(got) != len(AllScopes) {
			t.Errorf("ParseScopes(%q) = %d scopes, want all (%d)", in, len(got), len(AllScopes))
		}
	}
}

func TestParseScopesSubset(t *testing.T) {
	got := ParseScopes(`["workspace:read","session:read"]`)
	if len(got) != 2 || got[0] != ScopeWorkspaceRead || got[1] != ScopeSessionRead {
		t.Fatalf("unexpected subset: %v", got)
	}
}

func TestHasScope(t *testing.T) {
	stored := `["workspace:read"]`
	if !HasScope(stored, ScopeWorkspaceRead) {
		t.Error("should grant workspace:read")
	}
	if HasScope(stored, ScopeHookApprove) {
		t.Error("must NOT grant hook:approve")
	}
	// "all" grants everything.
	if !HasScope("all", ScopeHookApprove) || !HasScope("", ScopeFilesWrite) {
		t.Error("all/empty must grant everything")
	}
}

func TestNormalizeScopes(t *testing.T) {
	// Unknown scopes dropped; dedup; a full set collapses to "all".
	if got := NormalizeScopes([]string{"workspace:read", "bogus", "workspace:read"}); got != `["workspace:read"]` {
		t.Errorf("normalize subset = %q", got)
	}
	full := make([]string, len(AllScopes))
	for i, s := range AllScopes {
		full[i] = string(s)
	}
	if got := NormalizeScopes(full); got != "all" {
		t.Errorf("full set should collapse to all, got %q", got)
	}
	if got := NormalizeScopes([]string{"bogus"}); got != "all" {
		t.Errorf("all-invalid → all (nothing valid), got %q", got)
	}
}
