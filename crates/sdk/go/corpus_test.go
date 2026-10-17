package aikoql

// The §16 pin: the cross-language golden corpus spec exists at
// sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.
// Removing a case id is a detected coverage loss; a column mismatch is a
// wire-behavior drift (or an undocumented divergence — document it in the
// spec's note and re-stamp). The evaluation runs the real wire functions
// (normalizeCode/classifyID/parseVersion/decodeStreamNotify via the
// rpcResponse path) — the request-loop verdicts are restated inline,
// pointing at their aikoql.go lines.

import (
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

// The frozen §16 case ids (the corpus spec; one case removed = RED).
var corpusCaseIDs = []string{
	"corp-v01", "corp-v02", "corp-v03", "corp-v04", "corp-v05",
	"corp-e01", "corp-e02", "corp-e03", "corp-e04", "corp-e05",
	"corp-r01", "corp-r02", "corp-n01",
	"corp-s01", "corp-s02", "corp-s03",
	"corp-c01", "corp-c02", "corp-c03", "corp-c04",
}

// The frozen §16 surfaces.
var corpusSurfaces = map[string]bool{
	"version": true, "rpc_error": true, "response_id": true,
	"nonfinite": true, "notify": true, "correlation": true,
}

// The §16 stream the notify verdicts run against (the Go loop's
// streamID, aikoql.go:484).
const corpusStreamID = "s1"

type corpusSpec struct {
	Cases []struct {
		ID       string                     `json:"id"`
		Surface  string                     `json:"surface"`
		Input    json.RawMessage            `json:"input"`
		Expected map[string]json.RawMessage `json:"expected"`
	} `json:"cases"`
}

func readCorpus(t *testing.T) corpusSpec {
	t.Helper()
	raw, err := os.ReadFile("../../../sdk-fuzz-corpus/corpus.json")
	if err != nil {
		t.Fatalf("the §16 corpus is absent (../../../sdk-fuzz-corpus/corpus.json): %v — the D-16 golden-corpus slice is missing", err)
	}
	var spec corpusSpec
	if err := json.Unmarshal(raw, &spec); err != nil {
		t.Fatalf("the §16 corpus does not parse: %v", err)
	}
	return spec
}

// corrName renders a classifyID verdict as its frozen §16 name.
func corrName(c int) string {
	switch c {
	case corrSkip:
		return "skip"
	case corrMatch:
		return "match"
	case corrProtocol:
		return "protocol"
	}
	return "unknown"
}

// corpusEvaluate renders this SDK's classification of one corpus input as
// JSON, on the real wire functions.
func corpusEvaluate(surface string, input json.RawMessage) (string, error) {
	switch surface {
	case "version":
		var v string
		if err := json.Unmarshal(input, &v); err != nil {
			return "", err
		}
		out, err := json.Marshal(parseVersion(v))
		return string(out), err
	case "rpc_error":
		var frame string
		if err := json.Unmarshal(input, &frame); err != nil {
			return "", err
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(frame), &resp); err != nil {
			return `"reject"`, nil // the frame fails the struct parse — noise skip
		}
		if resp.Error == nil {
			return `"reject"`, nil
		}
		me := resp.Error.mcpError()
		out, err := json.Marshal(map[string]string{"code": me.Code, "message": me.Message})
		return string(out), err
	case "response_id", "nonfinite":
		var frame string
		if err := json.Unmarshal(input, &frame); err != nil {
			return "", err
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(frame), &resp); err != nil {
			return `"skip"`, nil // noise frame (aikoql.go:386-388)
		}
		out, err := json.Marshal(corrName(classifyID(1, resp.ID)))
		return string(out), err
	case "notify":
		var frame string
		if err := json.Unmarshal(input, &frame); err != nil {
			return "", err
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(frame), &resp); err != nil {
			return `{"verdict":"skip","pair":null}`, nil
		}
		// The stream loop's verdict, restated (aikoql.go:484-495).
		if resp.Method != "notifications/notify" {
			return `{"verdict":"skip","pair":null}`, nil
		}
		sid, done, err := decodeStreamNotify(resp.Params)
		if err != nil || sid != corpusStreamID {
			return `{"verdict":"skip","pair":null}`, nil
		}
		out, err := json.Marshal(map[string]any{
			"verdict": "yield",
			"pair":    map[string]any{"stream_id": sid, "done": done},
		})
		return string(out), err
	case "correlation":
		var pair struct {
			Want int64 `json:"want"`
			Got  int64 `json:"got"`
		}
		if err := json.Unmarshal(input, &pair); err != nil {
			return "", err
		}
		out, err := json.Marshal(corrName(classifyID(pair.Want, pair.Got)))
		return string(out), err
	}
	return "", nil
}

func TestCorpusPin(t *testing.T) {
	spec := readCorpus(t)
	byID := map[string]json.RawMessage{}
	surfaces := map[string]bool{}
	for _, c := range spec.Cases {
		byID[c.ID] = c.Expected["go"]
		surfaces[c.Surface] = true
	}
	for _, id := range corpusCaseIDs {
		if _, ok := byID[id]; !ok {
			t.Errorf("case %s is gone from the §16 corpus — a removed case is a coverage loss", id)
		}
	}
	for surface := range corpusSurfaces {
		if !surfaces[surface] {
			t.Errorf("surface %s has no cases in the §16 corpus", surface)
		}
	}
	for _, c := range spec.Cases {
		want, ok := c.Expected["go"]
		if !ok {
			t.Errorf("case %s has no go column in the §16 corpus", c.ID)
			continue
		}
		got, err := corpusEvaluate(c.Surface, c.Input)
		if err != nil {
			t.Errorf("case %s: evaluate: %v", c.ID, err)
			continue
		}
		var wantV, gotV any
		if err := json.Unmarshal(want, &wantV); err != nil {
			t.Errorf("case %s: bad go column: %v", c.ID, err)
			continue
		}
		if err := json.Unmarshal([]byte(got), &gotV); err != nil {
			t.Errorf("case %s: bad evaluation: %v", c.ID, err)
			continue
		}
		if !reflect.DeepEqual(gotV, wantV) {
			t.Errorf("case %s (%s): go column drift — got %s, want %s", c.ID, c.Surface, got, want)
		}
	}
}
