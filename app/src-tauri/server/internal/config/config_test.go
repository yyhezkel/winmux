package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestRotateCopyTruncate(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "insights.log")
	big := strings.Repeat("x", 2048)
	if err := os.WriteFile(p, []byte(big), 0o644); err != nil {
		t.Fatal(err)
	}
	rotateCopyTruncate(p, 1024)
	if fi, err := os.Stat(p); err != nil || fi.Size() != 0 {
		t.Fatalf("original should be truncated to 0, got size=%d err=%v", fi.Size(), err)
	}
	if rotated, err := os.ReadFile(p + ".1"); err != nil || len(rotated) != len(big) {
		t.Fatalf(".1 should hold the old contents (%d), got %d err=%v", len(big), len(rotated), err)
	}
	small := filepath.Join(dir, "small.log")
	_ = os.WriteFile(small, []byte("tiny"), 0o644)
	rotateCopyTruncate(small, 1024)
	if _, err := os.Stat(small + ".1"); !os.IsNotExist(err) {
		t.Fatal("small log should not have been rotated")
	}
}

func TestPruneIfOld(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "old.log")
	_ = os.WriteFile(p, []byte("stale"), 0o644)
	old := time.Now().Add(-48 * time.Hour)
	if err := os.Chtimes(p, old, old); err != nil {
		t.Fatal(err)
	}
	pruneIfOld(p, 24*time.Hour)
	if _, err := os.Stat(p); !os.IsNotExist(err) {
		t.Fatal("stale log should have been pruned")
	}
	fresh := filepath.Join(dir, "fresh.log")
	_ = os.WriteFile(fresh, []byte("new"), 0o644)
	pruneIfOld(fresh, 24*time.Hour)
	if _, err := os.Stat(fresh); err != nil {
		t.Fatal("fresh log should survive")
	}
}
