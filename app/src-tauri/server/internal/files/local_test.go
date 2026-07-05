package files

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func newSandbox(t *testing.T) (*LocalFiles, string) {
	t.Helper()
	base := t.TempDir()
	root := filepath.Join(base, "sandbox")
	lf, err := NewLocalFiles(root, 1024) // small max upload for the size test
	if err != nil {
		t.Fatalf("NewLocalFiles: %v", err)
	}
	return lf, base
}

// A path with `..` must never reach a file OUTSIDE the sandbox — it's collapsed
// to stay within root, so the escape attempt simply misses.
func TestTraversalCannotEscape(t *testing.T) {
	lf, base := newSandbox(t)
	// A secret sitting next to (outside) the sandbox root.
	secret := filepath.Join(base, "secret.txt")
	if err := os.WriteFile(secret, []byte("TOP-SECRET"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, p := range []string{"../secret.txt", "../../secret.txt", "..\\secret.txt", "/../secret.txt"} {
		data, _, err := lf.Read(p, 100)
		if err == nil && strings.Contains(string(data), "TOP-SECRET") {
			t.Fatalf("traversal %q leaked the out-of-sandbox secret", p)
		}
	}
}

// A symlink inside the sandbox pointing OUT of it must be rejected.
func TestSymlinkEscapeRejected(t *testing.T) {
	lf, base := newSandbox(t)
	secret := filepath.Join(base, "secret.txt")
	_ = os.WriteFile(secret, []byte("TOP-SECRET"), 0o644)
	link := filepath.Join(lf.Root(), "escape")
	if err := os.Symlink(secret, link); err != nil {
		t.Skipf("symlink not supported here: %v", err) // Windows w/o privilege
	}
	if _, _, err := lf.Read("escape", 100); err != ErrOutsideSandbox {
		t.Fatalf("symlink escape: want ErrOutsideSandbox, got %v", err)
	}
}

func TestWriteReadListDeleteRoundTrip(t *testing.T) {
	lf, _ := newSandbox(t)
	sum, size, err := lf.Write("dir/hello.txt", []byte("hi"))
	if err != nil {
		t.Fatalf("Write: %v", err)
	}
	if size != 2 || len(sum) != 64 {
		t.Fatalf("Write meta: size=%d sha=%q", size, sum)
	}
	data, truncated, err := lf.Read("dir/hello.txt", 100)
	if err != nil || truncated || string(data) != "hi" {
		t.Fatalf("Read: data=%q truncated=%v err=%v", data, truncated, err)
	}
	// List the sandbox root: the "dir" directory shows up, typed as dir.
	cwd, entries, err := lf.List("", 1)
	if err != nil || cwd != lf.Root() {
		t.Fatalf("List: cwd=%q err=%v", cwd, err)
	}
	if len(entries) != 1 || entries[0].Name != "dir" || entries[0].Type != "dir" {
		t.Fatalf("List entries unexpected: %+v", entries)
	}
	// depth=2 flattens the child in.
	_, deep, _ := lf.List("", 2)
	var sawChild bool
	for _, e := range deep {
		if e.Name == "dir/hello.txt" && e.Type == "file" {
			sawChild = true
		}
	}
	if !sawChild {
		t.Fatalf("depth=2 should surface dir/hello.txt: %+v", deep)
	}
	if err := lf.Delete("dir/hello.txt"); err != nil {
		t.Fatalf("Delete: %v", err)
	}
	if _, _, err := lf.Read("dir/hello.txt", 100); err != ErrNotFound {
		t.Fatalf("after delete: want ErrNotFound, got %v", err)
	}
}

func TestReadTruncation(t *testing.T) {
	lf, _ := newSandbox(t)
	_, _, _ = lf.Write("big.txt", []byte("0123456789"))
	data, truncated, err := lf.Read("big.txt", 4)
	if err != nil || !truncated || string(data) != "0123" {
		t.Fatalf("truncate: data=%q truncated=%v err=%v", data, truncated, err)
	}
}

func TestUploadSizeLimit(t *testing.T) {
	lf, _ := newSandbox(t) // maxUpload = 1024
	if _, _, err := lf.Write("ok.bin", make([]byte, 1024)); err != nil {
		t.Fatalf("1024 bytes should be allowed: %v", err)
	}
	if _, _, err := lf.Write("toobig.bin", make([]byte, 1025)); err == nil {
		t.Fatal("1025 bytes should exceed the 1024 max upload")
	}
}
