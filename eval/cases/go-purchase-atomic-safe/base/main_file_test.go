package main

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"testing"
)

func serveFile(t *testing.T, dir, name string) *httptest.ResponseRecorder {
	t.Helper()
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/file?f="+url.QueryEscape(name), nil)
	fileHandler(dir)(rec, req)
	return rec
}

func TestFileHandlerServesFilesInsideTheDataDir(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "report.txt"), []byte("hello"), 0o600); err != nil {
		t.Fatal(err)
	}
	rec := serveFile(t, dir, "report.txt")
	if rec.Code != http.StatusOK || rec.Body.String() != "hello" {
		t.Fatalf("got %d %q", rec.Code, rec.Body.String())
	}
}

func TestFileHandlerRejectsTraversalAndAbsolutePaths(t *testing.T) {
	dir := t.TempDir()
	for _, name := range []string{"", "../../etc/passwd", "/etc/passwd", "subdir/../../x", "..%2F..%2Fetc%2Fpasswd"} {
		decoded, _ := url.QueryUnescape(name)
		if rec := serveFile(t, dir, decoded); rec.Code != http.StatusBadRequest {
			t.Errorf("%q: got %d, want 400", name, rec.Code)
		}
	}
}

func TestFileHandlerDoesNotFollowSymlinksOutOfTheDataDir(t *testing.T) {
	dir, outside := t.TempDir(), t.TempDir()
	secret := filepath.Join(outside, "secret.txt")
	if err := os.WriteFile(secret, []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(secret, filepath.Join(dir, "link.txt")); err != nil {
		t.Skip("symlinks unsupported")
	}
	if rec := serveFile(t, dir, "link.txt"); rec.Code == http.StatusOK {
		t.Fatalf("symlink escape served %q", rec.Body.String())
	}
}

func TestFileHandlerMissingFileIsNotFoundWithoutLeakingPaths(t *testing.T) {
	dir := t.TempDir()
	rec := serveFile(t, dir, "missing.txt")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("got %d, want 404", rec.Code)
	}
	if body := rec.Body.String(); filepath.IsAbs(body) || len(body) > 20 {
		t.Fatalf("body leaks detail: %q", body)
	}
}
