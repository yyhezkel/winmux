package config

import (
	"os"
	"strings"
	"testing"
)

func TestMergePaths(t *testing.T) {
	sep := string(os.PathListSeparator)
	base := strings.Join([]string{"/usr/bin", "/bin"}, sep)
	got := MergePaths(base, []string{"/usr/bin", "/home/u/.local/bin", "", "/usr/local/bin"})
	want := strings.Join([]string{"/usr/bin", "/bin", "/home/u/.local/bin", "/usr/local/bin"}, sep)
	if got != want {
		t.Fatalf("MergePaths:\n got %q\nwant %q", got, want)
	}
	// empty base → just the (deduped, non-empty) extras.
	if g := MergePaths("", []string{"/a", "/a", "/b"}); g != "/a"+sep+"/b" {
		t.Fatalf("empty base merge: %q", g)
	}
}
