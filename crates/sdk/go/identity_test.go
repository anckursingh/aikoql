package aikoql

import (
	"os"
	"strings"
	"testing"
)

// The module path is the SDK's identity: it must be a path this project
// owns, never a squatted namespace, and it must equal repo + directory
// (github.com/anckursingh/aikoql/crates/sdk/go) or the Go proxy cannot
// find go.mod ("missing .../sdk/go/go.mod at revision sdk/go/v0.2.3").
// Renaming the module breaks every import, so the go.mod line is the one
// source of truth this test pins.
func TestModuleIdentity(t *testing.T) {
	const want = "module github.com/anckursingh/aikoql/crates/sdk/go"

	raw, err := os.ReadFile("go.mod")
	if err != nil {
		t.Fatalf("read go.mod: %v", err)
	}
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.HasPrefix(line, "module ") {
			if strings.TrimSpace(line) != want {
				t.Fatalf("go.mod declares %q, want %q", strings.TrimSpace(line), want)
			}
			return
		}
	}
	t.Fatalf("go.mod has no module line")
}
