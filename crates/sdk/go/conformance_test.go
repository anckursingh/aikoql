package aikoql

// D-11: the shared conformance runner (§7, §23). The runner executes the
// §23 canonical workload and the §7 category vectors from
// tests/sdk-conformance/ (plus the frozen protocol/test-vectors/) against a
// real server through this SDK, with the same expected results every
// language must produce. This test pins only the CLI contract; the vectors
// carry the semantics.

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestSDKConformance(t *testing.T) {
	script := filepath.Join("..", "..", "..", "scripts", "sdk-conformance.sh")
	if _, err := os.Stat(script); err != nil {
		t.Fatalf("sdk-conformance runner missing (%v) — D-11 RED", err)
	}
	if os.Getenv("AIKOQL_MCP_BIN") == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server conformance skipped")
	}
	out, err := exec.Command("bash", script, "--language", "go").CombinedOutput()
	if err != nil {
		t.Fatalf("sdk-conformance --language go failed: %v\n%s", err, out)
	}
}
