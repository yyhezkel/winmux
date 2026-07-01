package main

import "testing"

func TestArgAfter(t *testing.T) {
	args := []string{"winmux", "port-watch", "--workspace", "w_abc"}
	if got := argAfter(args, "--workspace"); got != "w_abc" {
		t.Fatalf("workspace: got %q", got)
	}
	if got := argAfter(args, "--missing"); got != "" {
		t.Fatalf("missing flag should be empty, got %q", got)
	}
	// Flag as the very last token has no value → empty, no panic.
	if got := argAfter([]string{"claude", "--session-id"}, "--session-id"); got != "" {
		t.Fatalf("trailing flag should be empty, got %q", got)
	}
}

func TestMarkDuplicates(t *testing.T) {
	// wA has 3 (keep the newest = etime 10), wB has 1 (keep), wC has 2.
	ws := []PortWatcher{
		{PID: 1, Workspace: "wA", EtimeSec: 300},
		{PID: 2, Workspace: "wA", EtimeSec: 10},
		{PID: 3, Workspace: "wA", EtimeSec: 200},
		{PID: 4, Workspace: "wB", EtimeSec: 50},
		{PID: 5, Workspace: "wC", EtimeSec: 500},
		{PID: 6, Workspace: "wC", EtimeSec: 5},
	}
	if dups := markDuplicates(ws); dups != 3 {
		t.Fatalf("expected 3 duplicates, got %d", dups)
	}
	dupOf := map[int32]bool{}
	for _, w := range ws {
		dupOf[w.PID] = w.Duplicate
	}
	// Newest per workspace must be kept.
	for _, keep := range []int32{2, 4, 6} {
		if dupOf[keep] {
			t.Fatalf("pid %d (newest for its workspace) should NOT be a duplicate", keep)
		}
	}
	for _, dup := range []int32{1, 3, 5} {
		if !dupOf[dup] {
			t.Fatalf("pid %d should be a duplicate", dup)
		}
	}
}
