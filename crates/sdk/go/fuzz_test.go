package aikoql

import (
	"os"
	"regexp"
	"testing"
)

// TestFuzzEstatePin — the §11 names must exist as native fuzz targets
// (the F-04 pattern: the estate's own pin detects a removed target,
// never a silent coverage loss).
func TestFuzzEstatePin(t *testing.T) {
	src, err := os.ReadFile("fuzz_test.go")
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{
		"FuzzParseRPCResponse",
		"FuzzParseMcpError",
		"FuzzDecodeToolEnvelope",
		"FuzzDecodeStreamChunk",
		"FuzzVersionParser",
		"FuzzFrameDecoder",
		"FuzzRequestIDCorrelation",
		"FuzzErrorMapping",
	} {
		re := regexp.MustCompile(`(?m)^func ` + name + `\(f \*testing\.F\)`)
		if !re.Match(src) {
			t.Errorf("§11 fuzz target missing: %s", name)
		}
	}
}
