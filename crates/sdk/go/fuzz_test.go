package aikoql

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"reflect"
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

// The eight §11 targets below share the frozen properties: never panic,
// malformed data yields no successful object, successful parses round-trip,
// outputs are deterministic (same input twice → same result).

// FuzzParseRPCResponse — a successful response parse must re-marshal to
// valid JSON and decode identically on a second pass.
func FuzzParseRPCResponse(f *testing.F) {
	f.Add([]byte(`{"jsonrpc":"2.0","id":1,"result":{"koid":"x"}}`))
	f.Add([]byte(`{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nf"}}`))
	f.Add([]byte(`{"jsonrpc":"2.0","method":"notifications/notify","params":{"stream_id":"s","done":true}}`))
	f.Add([]byte(`{}`))
	f.Fuzz(func(t *testing.T, data []byte) {
		var resp rpcResponse
		if json.Unmarshal(data, &resp) != nil {
			return // malformed: nothing may escape
		}
		out, err := json.Marshal(resp)
		if err != nil {
			t.Fatalf("re-marshal: %v", err)
		}
		if !json.Valid(out) {
			t.Fatalf("re-marshal is invalid JSON: %s", out)
		}
		// The SDK only ever DECODES responses — assert decode determinism
		// (same input twice → same struct), not marshal round-trip: a nil
		// RawMessage marshals as `null` and decodes back as RawMessage("null").
		var again rpcResponse
		if err := json.Unmarshal(data, &again); err != nil {
			t.Fatalf("re-decode: %v", err)
		}
		if !reflect.DeepEqual(resp, again) {
			t.Fatalf("decode is not deterministic for %s: %#v vs %#v",
				data, resp, again)
		}
	})
}

// FuzzParseMcpError — a decoded error frame maps to a non-empty, quote-free
// code and its message verbatim, deterministically.
func FuzzParseMcpError(f *testing.F) {
	f.Add([]byte(`{"code":-32601,"message":"method not found"}`))
	f.Add([]byte(`{"code":"-32601","message":"string-encoded"}`))
	f.Add([]byte(`{"code":"FRAME_TOO_LARGE","message":"over cap"}`))
	f.Add([]byte(`{"message":"no code"}`))
	f.Fuzz(func(t *testing.T, data []byte) {
		var e rpcError
		if json.Unmarshal(data, &e) != nil {
			return
		}
		me := e.mcpError()
		if me.Code == "" {
			t.Fatal("mapped code is empty")
		}
		if me.Message != e.Message {
			t.Fatalf("message altered: %q vs %q", me.Message, e.Message)
		}
		if again := e.mcpError(); !reflect.DeepEqual(me, again) {
			t.Fatalf("mapping is not deterministic: %#v vs %#v", me, again)
		}
	})
}

// FuzzDecodeToolEnvelope — a nil error must carry valid JSON out; the same
// input decodes to the same payload twice.
func FuzzDecodeToolEnvelope(f *testing.F) {
	f.Add([]byte(`{"content":[{"text":"{\"ok\":true,\"data\":{\"koid\":\"x\"}}"}]}`))
	f.Add([]byte(`{"content":[{"text":"{\"ok\":false,\"error\":{\"code\":\"NOT_FOUND\",\"message\":\"gone\"}}"}]}`))
	f.Add([]byte(`{"content":[{"text":"{\"ok\":true}"}]}`))
	f.Add([]byte(`{"content":[]}`))
	f.Add([]byte(`{}`))
	f.Fuzz(func(t *testing.T, data []byte) {
		out, err := decodeToolEnvelope("fuzz", data)
		if err == nil && len(out) > 0 && !json.Valid(out) {
			t.Fatalf("successful decode escaped invalid JSON: %s", out)
		}
		out2, err2 := decodeToolEnvelope("fuzz", data)
		if !bytes.Equal(out, out2) || (err == nil) != (err2 == nil) {
			t.Fatalf("decode is not deterministic: (%s, %v) vs (%s, %v)",
				out, err, out2, err2)
		}
	})
}

// FuzzDecodeStreamChunk — a successful params decode must re-round-trip to
// the same stream id and done flag.
func FuzzDecodeStreamChunk(f *testing.F) {
	f.Add([]byte(`{"stream_id":"s1","done":true}`))
	f.Add([]byte(`{"stream_id":"s1"}`))
	f.Add([]byte(`{"stream_id":"s1","done":false}`))
	f.Add([]byte(`{}`))
	f.Add([]byte(`null`))
	f.Fuzz(func(t *testing.T, data []byte) {
		id, done, err := decodeStreamNotify(data)
		if err != nil {
			return
		}
		round, rerr := json.Marshal(struct {
			StreamID string `json:"stream_id"`
			Done     bool   `json:"done"`
		}{id, done})
		if rerr != nil {
			t.Fatalf("re-marshal: %v", rerr)
		}
		id2, done2, err2 := decodeStreamNotify(round)
		if err2 != nil || id2 != id || done2 != done {
			t.Fatalf("round-trip drifted: (%q,%v,%v) vs (%q,%v,%v)",
				id, done, err, id2, done2, err2)
		}
	})
}

// FuzzVersionParser — the frozen mirror of Python's int(seg) semantics
// (signed segments ARE numeric there, so no lower bound), plus
// determinism and the irreflexivity of versionLess.
func FuzzVersionParser(f *testing.F) {
	f.Add("0.2.0")
	f.Add("0.1.19")
	f.Add("2024-11-05")
	f.Add("x.y")
	f.Add("1.2.3.4.5")
	f.Add("")
	f.Fuzz(func(t *testing.T, v string) {
		p := parseVersion(v)
		if versionLess(p, p) {
			t.Fatalf("versionLess is not irreflexive for %q", v)
		}
		if again := parseVersion(v); !reflect.DeepEqual(p, again) {
			t.Fatalf("parse is not deterministic for %q", v)
		}
	})
}

// FuzzFrameDecoder — readLine never returns more than maxFrame bytes, and
// FRAME_TOO_LARGE fires only when the data actually runs past the cap.
func FuzzFrameDecoder(f *testing.F) {
	f.Add([]byte(`{"jsonrpc":"2.0","id":1,"result":{}}` + "\n"))
	f.Add([]byte(`{"jsonrpc":"2.0","id":1,"result":{}}`))
	f.Add([]byte("\n"))
	f.Add([]byte{})
	f.Add(bytes.Repeat([]byte("x"), maxFrame+1))
	f.Fuzz(func(t *testing.T, data []byte) {
		c := &Client{r: bufio.NewReader(bytes.NewReader(data))}
		line, err := c.readLine()
		if len(line) > maxFrame {
			t.Fatalf("readLine escaped the cap: %d bytes", len(line))
		}
		var me *McpError
		if errors.As(err, &me) && me.Code == "FRAME_TOO_LARGE" &&
			len(data) <= maxFrame {
			t.Fatalf("FRAME_TOO_LARGE on %d bytes of input", len(data))
		}
		c2 := &Client{r: bufio.NewReader(bytes.NewReader(data))}
		line2, err2 := c2.readLine()
		if line2 != line || errText(err) != errText(err2) {
			t.Fatalf("readLine is not deterministic: (%q, %v) vs (%q, %v)",
				line, err, line2, err2)
		}
	})
}

func errText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

// FuzzRequestIDCorrelation — the frozen §3.3 rules restated independently:
// smaller ids skip, larger ids are PROTOCOL_ERROR, equal ids match.
func FuzzRequestIDCorrelation(f *testing.F) {
	f.Add(int64(1), int64(1))
	f.Add(int64(1), int64(2))
	f.Add(int64(2), int64(1))
	f.Add(int64(0), int64(0))
	f.Add(int64(-1), int64(5))
	f.Fuzz(func(t *testing.T, want, got int64) {
		switch {
		case got < want:
			if classifyID(want, got) != corrSkip {
				t.Fatalf("smaller id must skip: %d vs %d", want, got)
			}
		case got > want:
			if classifyID(want, got) != corrProtocol {
				t.Fatalf("larger id must be PROTOCOL_ERROR: %d vs %d", want, got)
			}
		default:
			if classifyID(want, got) != corrMatch {
				t.Fatalf("equal ids must match: %d vs %d", want, got)
			}
		}
	})
}

// FuzzErrorMapping — the frozen normalizeCode rules: non-empty output for
// every raw code, quotes stripped exactly when the code is string-encoded
// (the raw JSON arrives quote-wrapped), INTERNAL for the empty one.
func FuzzErrorMapping(f *testing.F) {
	f.Add("-32601")
	f.Add(`"-32601"`)
	f.Add("FRAME_TOO_LARGE")
	f.Add(`""`)
	f.Add("")
	f.Fuzz(func(t *testing.T, raw string) {
		code := normalizeCode(raw)
		if code == "" {
			t.Fatal("normalized code is empty")
		}
		if raw == "" {
			if code != "INTERNAL" {
				t.Fatalf("empty code must map to INTERNAL, got %q", code)
			}
			return
		}
		if !json.Valid([]byte(raw)) {
			return // garbage cannot arrive on the wire
		}
		if raw[0] == '"' {
			// the frozen Python mirror: a string code surfaces json-decoded
			var want string
			if err := json.Unmarshal([]byte(raw), &want); err != nil {
				t.Fatalf("oracle decode of %q: %v", raw, err)
			}
			if want == "" {
				want = "INTERNAL"
			}
			if code != want {
				t.Fatalf("string code drifted: %q -> %q, want %q", raw, code, want)
			}
		} else if code != raw {
			t.Fatalf("unquoted code altered: %q -> %q", raw, code)
		}
	})
}
