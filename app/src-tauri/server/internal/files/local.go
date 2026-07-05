// Package files serves the sandboxed shared-folder / directory-picker API
// (/api/v2/files/*, PHASE-77-DESIGN §4.2). LocalFiles is a core.FilesProvider
// backed by a single root directory; every request path is confined to that
// root — `..` is collapsed against "/" before joining, and symlink targets that
// escape the root are rejected. The daemon runs as the user, so filesystem
// permissions are the final backstop.
package files

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"winmux-server/internal/core"
)

var (
	// ErrOutsideSandbox is returned for any path that would escape the root.
	ErrOutsideSandbox = errors.New("path escapes the sandbox root")
	// ErrNotFound is returned for a missing path.
	ErrNotFound = errors.New("not found")
	// ErrIsDir is returned when a file operation targets a directory.
	ErrIsDir = errors.New("path is a directory")
)

// DefaultMaxUpload caps a single upload (configurable via NewLocalFiles).
const DefaultMaxUpload int64 = 100 << 20 // 100 MB

// defaultReadCap bounds Read when the caller doesn't ask for a specific size.
const defaultReadCap int64 = 1 << 20 // 1 MB

// LocalFiles is a root-confined core.FilesProvider.
type LocalFiles struct {
	root      string // absolute, symlink-resolved
	maxUpload int64
}

// NewLocalFiles confines all operations to root (created if missing). maxUpload
// <= 0 uses DefaultMaxUpload.
func NewLocalFiles(root string, maxUpload int64) (*LocalFiles, error) {
	abs, err := filepath.Abs(root)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(abs, 0o755); err != nil {
		return nil, err
	}
	// Canonicalise the root so containment comparisons use real paths.
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		abs = resolved
	}
	if maxUpload <= 0 {
		maxUpload = DefaultMaxUpload
	}
	return &LocalFiles{root: abs, maxUpload: maxUpload}, nil
}

// Root returns the absolute sandbox root.
func (l *LocalFiles) Root() string { return l.root }

// within reports whether p is at or below the sandbox root.
func (l *LocalFiles) within(p string) bool {
	rel, err := filepath.Rel(l.root, p)
	if err != nil {
		return false
	}
	return rel == "." || (rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)))
}

// resolve maps a client-supplied path to an absolute path guaranteed inside the
// sandbox. The client path is treated as rooted at the sandbox: any `..` is
// collapsed against "/" (so it can never climb above root) before joining. For
// an existing target we also reject a symlink whose destination escapes; for a
// not-yet-existing target (upload) we check the nearest existing ancestor.
func (l *LocalFiles) resolve(p string) (string, error) {
	clean := filepath.Clean("/" + strings.ReplaceAll(p, "\\", "/"))
	full := filepath.Join(l.root, clean)
	if !l.within(full) {
		return "", ErrOutsideSandbox
	}
	// Walk up to the nearest path that exists, EvalSymlinks it, re-check.
	probe := full
	for {
		if _, err := os.Lstat(probe); err == nil {
			break
		}
		parent := filepath.Dir(probe)
		if parent == probe {
			return full, nil // nothing exists yet up to root; join already contained
		}
		probe = parent
	}
	resolved, err := filepath.EvalSymlinks(probe)
	if err == nil && !l.within(resolved) {
		return "", ErrOutsideSandbox
	}
	return full, nil
}

func entryOf(name string, fi os.FileInfo) core.FileEntry {
	t, size := "file", fi.Size()
	if fi.IsDir() {
		t, size = "dir", 0
	}
	return core.FileEntry{Name: name, Type: t, Size: size, Modified: fi.ModTime().Unix()}
}

// List returns the resolved cwd + entries. depth 2 flattens one level of
// children (name carries the "sub/child" relative path). Dirs sort before files.
func (l *LocalFiles) List(p string, depth int) (string, []core.FileEntry, error) {
	full, err := l.resolve(p)
	if err != nil {
		return "", nil, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return "", nil, ErrNotFound
	}
	if !fi.IsDir() {
		return full, []core.FileEntry{entryOf(fi.Name(), fi)}, nil
	}
	if depth < 1 {
		depth = 1
	}
	if depth > 2 {
		depth = 2
	}
	des, err := os.ReadDir(full)
	if err != nil {
		return "", nil, err
	}
	entries := []core.FileEntry{}
	for _, de := range des {
		info, e := de.Info()
		if e != nil {
			continue
		}
		entries = append(entries, entryOf(de.Name(), info))
		if depth == 2 && de.IsDir() {
			if sub, e := os.ReadDir(filepath.Join(full, de.Name())); e == nil {
				for _, se := range sub {
					if si, e := se.Info(); e == nil {
						entries = append(entries, entryOf(de.Name()+"/"+se.Name(), si))
					}
				}
			}
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		if (entries[i].Type == "dir") != (entries[j].Type == "dir") {
			return entries[i].Type == "dir"
		}
		return entries[i].Name < entries[j].Name
	})
	return full, entries, nil
}

// Read returns up to maxBytes of a file (default cap when maxBytes <= 0).
func (l *LocalFiles) Read(p string, maxBytes int64) ([]byte, bool, error) {
	full, err := l.resolve(p)
	if err != nil {
		return nil, false, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return nil, false, ErrNotFound
	}
	if fi.IsDir() {
		return nil, false, ErrIsDir
	}
	if maxBytes <= 0 {
		maxBytes = defaultReadCap
	}
	f, err := os.Open(full)
	if err != nil {
		return nil, false, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, maxBytes+1))
	if err != nil {
		return nil, false, err
	}
	truncated := int64(len(data)) > maxBytes
	if truncated {
		data = data[:maxBytes]
	}
	return data, truncated, nil
}

// Write creates/overwrites a file atomically and returns its sha256 + size.
func (l *LocalFiles) Write(p string, data []byte) (string, int64, error) {
	if int64(len(data)) > l.maxUpload {
		return "", 0, fmt.Errorf("upload exceeds max size (%d bytes)", l.maxUpload)
	}
	full, err := l.resolve(p)
	if err != nil {
		return "", 0, err
	}
	if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
		return "", 0, err
	}
	tmp := full + ".winmux-tmp"
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return "", 0, err
	}
	if err := os.Rename(tmp, full); err != nil {
		_ = os.Remove(tmp)
		return "", 0, err
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), int64(len(data)), nil
}

// Delete removes a file or an EMPTY directory (never recursive — safety).
func (l *LocalFiles) Delete(p string) error {
	full, err := l.resolve(p)
	if err != nil {
		return err
	}
	if _, err := os.Stat(full); err != nil {
		return ErrNotFound
	}
	return os.Remove(full) // errors on a non-empty directory
}

// Open streams a file for download; the caller closes the ReadCloser.
func (l *LocalFiles) Open(p string) (io.ReadCloser, int64, error) {
	full, err := l.resolve(p)
	if err != nil {
		return nil, 0, err
	}
	fi, err := os.Stat(full)
	if err != nil {
		return nil, 0, ErrNotFound
	}
	if fi.IsDir() {
		return nil, 0, ErrIsDir
	}
	f, err := os.Open(full)
	if err != nil {
		return nil, 0, err
	}
	return f, fi.Size(), nil
}
