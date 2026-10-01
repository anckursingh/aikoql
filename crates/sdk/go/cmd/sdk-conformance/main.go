// Command sdk-conformance is the Go adapter for the shared conformance
// runner (D-11, §7/§23). It executes the language-neutral vectors from
// tests/sdk-conformance/ (the §23 canonical workload + the 13 §7 category
// dirs) and protocol/test-vectors/ against a real aikoql-mcp server through
// this SDK, then checks every assert/assert_any/expect_error. The expected
// results are the vectors themselves — every SDK produces the same
// transcript.
//
// Run through scripts/sdk-conformance.sh, or directly:
//
//	go run ./cmd/sdk-conformance -bin <aikoql-mcp> -vectors <dir> -protocol <dir>
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"sort"
	"strconv"
	"strings"
	"time"

	aikoql "github.com/anckursingh/aikoql/sdk/go"
)

type vectorFile struct {
	Name       string `json:"name"`
	Operations []map[string]any
}

func dotGet(obj any, path string) (any, error) {
	for _, part := range strings.Split(path, ".") {
		switch v := obj.(type) {
		case map[string]any:
			var ok bool
			if obj, ok = v[part]; !ok {
				return nil, fmt.Errorf("no key %q", part)
			}
		case []any:
			idx, err := strconv.Atoi(part)
			if err != nil || idx < 0 || idx >= len(v) {
				return nil, fmt.Errorf("no index %q", part)
			}
			obj = v[idx]
		default:
			return nil, fmt.Errorf("cannot descend into %T at %q", obj, part)
		}
	}
	return obj, nil
}

func toMap(v any) map[string]any {
	raw, err := json.Marshal(v)
	if err != nil {
		panic(err)
	}
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		panic(err)
	}
	return m
}

func mapCode(code string) string {
	// Wire surfaces → SDK-012 codes: the server's -32001 token rejection
	// is AUTHENTICATION_FAILED to the caller.
	if code == "-32001" {
		return "AUTHENTICATION_FAILED"
	}
	return code
}

type runner struct {
	addr   string
	token  string
	client *aikoql.Client
	vars   map[string]any
	last   string
}

func (r *runner) connect(token string) error {
	c, err := aikoql.Dial(context.Background(), r.addr, aikoql.WithToken(token))
	if err != nil {
		return err
	}
	if err := c.Initialize(context.Background()); err != nil {
		_ = c.Close()
		return err
	}
	r.client = c
	return nil
}

// runOp executes one op and returns the normalized result (JSON numbers
// are float64 everywhere, so vector expectations compare cleanly).
func (r *runner) runOp(op map[string]any) (map[string]any, error) {
	ctx := context.Background()
	name, _ := op["op"].(string)
	koid := r.ref(op["koid"])
	switch name {
	case "connect":
		tok := r.token
		if t, ok := op["token"].(string); ok {
			tok = t
		}
		if err := r.connect(tok); err != nil {
			return nil, err
		}
		return map[string]any{}, nil
	case "close":
		if err := r.client.Close(); err != nil {
			return nil, err
		}
		return map[string]any{}, nil
	case "health":
		raw, err := r.client.Health(ctx)
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	case "metrics":
		m, err := r.client.Metrics(ctx)
		if err != nil {
			return nil, err
		}
		return toMap(m), nil
	case "remember":
		rem, err := r.client.Remember(ctx, aikoql.RememberParams{
			TypeName:   op["type"].(string),
			Properties: props(op["properties"]),
		})
		if err != nil {
			return nil, err
		}
		r.last = rem.KOID
		return toMap(rem), nil
	case "update":
		rem, err := r.client.Remember(ctx, aikoql.RememberParams{
			TypeName:   op["type"].(string),
			KOID:       or(koid, r.last),
			Properties: props(op["properties"]),
		})
		if err != nil {
			return nil, err
		}
		r.last = rem.KOID
		return toMap(rem), nil
	case "get":
		ko, err := r.client.Get(ctx, or(koid, r.last), "")
		if err != nil {
			return nil, err
		}
		return toMap(ko), nil
	case "delete":
		raw, err := r.client.Forget(ctx, or(koid, r.last), "tombstone", "")
		if err != nil {
			return nil, err
		}
		m := rawMap(raw)
		r.last, _ = m["koid"].(string)
		return m, nil
	case "query":
		if stream, _ := op["stream"].(bool); stream {
			var chunks []any
			err := r.client.AikoqlStream(ctx, op["query"].(string), "",
				func(chunk json.RawMessage) error {
					chunks = append(chunks, rawMap(chunk))
					return nil
				})
			if err != nil {
				return nil, err
			}
			return map[string]any{"chunks": chunks}, nil
		}
		raw, err := r.client.Aikoql(ctx, op["query"].(string), "")
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	case "relate":
		raw, err := r.client.Relate(ctx, r.ref(op["from"]), r.ref(op["to"]),
			op["rel_type"].(string), "")
		if err != nil {
			return nil, err
		}
		m := rawMap(raw)
		r.last, _ = m["koid"].(string)
		return m, nil
	case "traverse":
		relType, _ := op["rel_type"].(string)
		depth := 1
		if d, ok := op["depth"].(float64); ok {
			depth = int(d)
		}
		raw, err := r.client.Traverse(ctx, koid, relType, "", depth)
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	case "find_similar":
		p := aikoql.FindSimilarParams{}
		if t, ok := op["text"].(string); ok {
			p.Text = t
		}
		if w, ok := op["wait_for_freshness_ms"].(float64); ok {
			ms := int64(w)
			p.WaitForFreshnessMs = &ms
		}
		hits, err := r.client.FindSimilar(ctx, p)
		if err != nil {
			return nil, err
		}
		return toMap(struct {
			Results []aikoql.ScoredKO `json:"results"`
		}{hits}), nil
	case "begin":
		tx, err := r.client.Begin(ctx)
		if err != nil {
			return nil, err
		}
		r.vars[op["as"].(string)] = tx // txn handles and koids share the map
		return map[string]any{"txn_id": tx.ID}, nil
	case "execute":
		tx, _ := r.vars[op["txn"].(string)[1:]].(*aikoql.Tx)
		if err := tx.Execute(ctx, aikoql.StagedOp{
			Action:     op["action"].(string),
			TypeName:   str(op["type"]),
			Properties: props(op["properties"]),
		}); err != nil {
			return nil, err
		}
		return map[string]any{}, nil
	case "commit":
		tx, _ := r.vars[op["txn"].(string)[1:]].(*aikoql.Tx)
		res, err := tx.Commit(ctx)
		if err != nil {
			return nil, err
		}
		return toMap(res), nil
	case "rollback":
		tx, _ := r.vars[op["txn"].(string)[1:]].(*aikoql.Tx)
		if err := tx.Rollback(ctx); err != nil {
			return nil, err
		}
		return map[string]any{"rolled_back": true}, nil
	case "explain":
		raw, err := r.client.Explain(ctx, koid, "", nil)
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	case "trace":
		raw, err := r.client.Trace(ctx, koid, "")
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	case "discover_schema":
		raw, err := r.client.DiscoverSchema(ctx)
		if err != nil {
			return nil, err
		}
		return rawMap(raw), nil
	}
	return nil, fmt.Errorf("op %q has no adapter arm", name)
}

// ref resolves "$name" against the var map; anything else passes through.
func (r *runner) ref(v any) string {
	s, _ := v.(string)
	if strings.HasPrefix(s, "$") {
		if koid, ok := r.vars[s[1:]].(string); ok {
			return koid
		}
	}
	return s
}

func (r *runner) capture(op map[string]any, result map[string]any) {
	as, _ := op["as"].(string)
	if as == "" {
		return
	}
	if k, ok := result["koid"].(string); ok {
		r.vars[as] = k
	} else if results, ok := result["results"].([]any); ok && len(results) > 0 {
		if first, ok := results[0].(map[string]any); ok {
			if k, ok := first["koid"].(string); ok {
				r.vars[as] = k
			}
		}
	}
}

func (r *runner) check(op map[string]any, result map[string]any) error {
	if asserts, ok := op["assert"].(map[string]any); ok {
		for path, want := range asserts {
			got, err := dotGet(result, path)
			if err != nil {
				return fmt.Errorf("assert %s: %v", path, err)
			}
			if s, ok := want.(string); ok && strings.HasPrefix(s, "$") {
				want = r.ref(s)
			}
			// DeepEqual, not != — JSON arrays/maps decode to uncomparable
			// []any/map[string]any on both sides.
			if !reflect.DeepEqual(got, want) {
				return fmt.Errorf("assert %s: expected %v, got %v", path, want, got)
			}
		}
	}
	if aa, ok := op["assert_any"].(map[string]any); ok {
		items, err := dotGet(result, aa["path"].(string))
		if err != nil {
			return fmt.Errorf("assert_any %s: %v", aa["path"], err)
		}
		list, _ := items.([]any)
		found := false
		switch match := aa["match"].(type) {
		case map[string]any:
			for _, e := range list {
				m, _ := e.(map[string]any)
				all := true
				for p, v := range match {
					got, err := dotGet(m, p)
					if err != nil || got != v {
						all = false
						break
					}
				}
				if all {
					found = true
					break
				}
			}
		default:
			for _, e := range list {
				if e == match {
					found = true
					break
				}
			}
		}
		if !found {
			return fmt.Errorf("assert_any %s: no element matches %v",
				aa["path"], aa["match"])
		}
	}
	return nil
}

func (r *runner) runVector(ops []map[string]any) error {
	if err := r.connect(r.token); err != nil {
		return err
	}
	defer func() {
		if r.client != nil {
			_ = r.client.Close()
		}
	}()
	r.vars, r.last = map[string]any{}, ""
	for i, op := range ops {
		expect, _ := op["expect_error"].(string)
		result, err := r.runOp(op)
		if err != nil {
			code := ""
			var me *aikoql.McpError
			if errors.As(err, &me) {
				code = mapCode(me.Code)
			} else {
				code = fmt.Sprintf("%T: %v", err, err)
			}
			if expect == code {
				continue
			}
			return fmt.Errorf("op %d %v: expected error %s, got %s: %v",
				i, op["op"], expect, code, err)
		}
		if expect != "" {
			return fmt.Errorf("op %d %v: expected error %s, none raised",
				i, op["op"], expect)
		}
		r.capture(op, result)
		if err := r.check(op, result); err != nil {
			return fmt.Errorf("op %d %v: %v", i, op["op"], err)
		}
	}
	return nil
}

func loadVectors(dir string) ([]vectorFile, error) {
	var paths []string
	err := filepath.Walk(dir, func(p string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() && strings.HasSuffix(p, ".json") {
			paths = append(paths, p)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	var out []vectorFile
	for _, p := range paths {
		raw, err := os.ReadFile(p)
		if err != nil {
			return nil, err
		}
		var vf vectorFile
		if err := json.Unmarshal(raw, &vf); err != nil {
			return nil, fmt.Errorf("%s: %w", p, err)
		}
		out = append(out, vf)
	}
	return out, nil
}

// spawnServer is the integration_test pattern: a probed free port and a
// db path that does not exist (the server auto-creates it as aikoql-v2).
func spawnServer(bin, token string) (proc *exec.Cmd, addr, db string, err error) {
	probe, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, "", "", err
	}
	port := probe.Addr().(*net.TCPAddr).Port
	_ = probe.Close()
	dir, err := os.MkdirTemp("", "conformance-")
	if err != nil {
		return nil, "", "", err
	}
	db = filepath.Join(dir, "db.redb") // does not exist → auto-create
	addr = net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	proc = exec.Command(bin, "serve", db, "--listen", addr,
		"--tcp-token", token+"::admin")
	// Stdout/Stderr stay nil (null device): an inherited pipe would let an
	// orphaned server hold a pipe-capturing parent open on failure.
	if err := proc.Start(); err != nil {
		return nil, "", "", err
	}
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		conn, err := net.DialTimeout("tcp", addr, 500*time.Millisecond)
		if err == nil {
			_ = conn.Close()
			return proc, addr, db, nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	_ = proc.Process.Kill()
	return nil, "", "", fmt.Errorf("server did not come up on %s", addr)
}

func rawMap(raw json.RawMessage) map[string]any {
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		panic(err)
	}
	return m
}

func props(v any) map[string]any {
	m, _ := v.(map[string]any)
	return m
}

func str(v any) string {
	s, _ := v.(string)
	return s
}

func or(a, b string) string {
	if a != "" {
		return a
	}
	return b
}

func main() {
	bin := flag.String("bin", "", "path to the aikoql-mcp binary")
	vectors := flag.String("vectors", "", "tests/sdk-conformance directory")
	protocol := flag.String("protocol", "", "protocol/test-vectors directory")
	token := flag.String("token", "conformance", "the --tcp-token TOKEN segment")
	flag.Parse()
	if err := run(*bin, *vectors, *protocol, *token); err != nil {
		fmt.Fprintln(os.Stderr, "sdk-conformance (go):", err)
		os.Exit(1)
	}
}

// run holds the whole leg so the server teardown defer runs on EVERY
// failure path — os.Exit would skip it and orphan the server (which, with
// an inherited stderr, also wedges a pipe-capturing parent).
func run(bin, vectors, protocol, token string) error {
	proc, addr, db, err := spawnServer(bin, token)
	if err != nil {
		return err
	}
	defer func() {
		_ = proc.Process.Kill()
		_ = proc.Wait()
		_ = os.RemoveAll(filepath.Dir(db))
		_ = os.Remove(db + ".audit.log")
	}()

	r := &runner{addr: addr, token: token}
	dirs := []string{protocol, vectors}
	vectorsRun, opsRun := 0, 0
	for _, dir := range dirs {
		files, err := loadVectors(dir)
		if err != nil {
			return err
		}
		for _, vf := range files {
			if err := r.runVector(vf.Operations); err != nil {
				return fmt.Errorf("%s: %v", vf.Name, err)
			}
			vectorsRun++
			opsRun += len(vf.Operations)
			fmt.Printf("  ok %s (%d ops)\n", vf.Name, len(vf.Operations))
		}
	}
	fmt.Printf("sdk-conformance (go): %d vectors, %d ops — all passed\n",
		vectorsRun, opsRun)
	return nil
}
