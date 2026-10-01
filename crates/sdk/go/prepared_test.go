package aikoql

// D-10: the §3.6 prepared statement and the §22 lifecycle spec.
//
// prepare/bind/execute/close. The initial implementation compiles the
// AikoQL query client-side on every execute — the abstraction precedes a
// native prepare protocol, so the statement holds no server-side plan
// (nothing to invalidate, nothing lost across a server restart).
// Scripted-server legs pin the bind/validate/substitute mechanics; the
// real-server legs (skipped without AIKOQL_MCP_BIN) pin transaction
// interaction and the re-compile after a restart.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"testing"
	"time"
)

// preparedResponder answers aikoql + remember and logs every call (the
// schema-invalidation leg needs both surfaces).
func preparedResponder(t *testing.T, log *[]txnCall) func(t *testing.T, req rpcFrame) []rpcFrame {
	t.Helper()
	return func(t *testing.T, req rpcFrame) []rpcFrame {
		var p txnCall
		if err := json.Unmarshal(req.Params, &p); err != nil {
			t.Fatalf("request params: %v", err)
		}
		*log = append(*log, p)
		var data any
		switch p.Name {
		case "aikoql":
			data = map[string]any{"results": []any{map[string]any{
				"koid": "k1", "type_name": "person",
				"properties": map[string]any{"name": "ada"},
			}}}
		case "remember":
			data = map[string]any{"koid": "k2", "version": 1}
		default:
			t.Fatalf("unexpected tool %q", p.Name)
		}
		return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
			Result: toolResult(map[string]any{"ok": true, "data": data})}}
	}
}

// invalidQueryResponder rejects every statement at compile time (the
// server-side arm of the invalid-statement leg).
func invalidQueryResponder(t *testing.T) func(t *testing.T, req rpcFrame) []rpcFrame {
	t.Helper()
	return func(t *testing.T, req rpcFrame) []rpcFrame {
		var p txnCall
		if err := json.Unmarshal(req.Params, &p); err != nil {
			t.Fatalf("request params: %v", err)
		}
		if p.Name != "aikoql" {
			t.Fatalf("unexpected tool %q", p.Name)
		}
		return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
			Result: toolResult(map[string]any{"ok": false,
				"error": map[string]any{"code": "INVALID_QUERY",
					"message": "could not compile the query"}})}}
	}
}

// aikoqlQueries extracts the substituted queries from the call log.
func aikoqlQueries(log []txnCall) []string {
	var queries []string
	for _, call := range log {
		if call.Name == "aikoql" {
			queries = append(queries, call.Args["query"].(string))
		}
	}
	return queries
}

func mcpErrorCode(t *testing.T, err error) string {
	t.Helper()
	var me *McpError
	if !errors.As(err, &me) {
		t.Fatalf("expected an McpError, got %v", err)
	}
	return me.Code
}

func TestPreparedLifecycleBindExecuteExecuteClose(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	ctx := context.Background()

	ps, err := c.Prepare(ctx, "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	bound := ps.Bind(map[string]any{"who": "ada"})
	r1, err := bound.Execute(ctx)
	if err != nil {
		t.Fatalf("Execute 1: %v", err)
	}
	r2, err := bound.Execute(ctx) // a bound statement is re-executable
	if err != nil {
		t.Fatalf("Execute 2: %v", err)
	}
	for _, raw := range []json.RawMessage{r1, r2} {
		var out struct {
			Results []struct {
				Properties map[string]any `json:"properties"`
			} `json:"results"`
		}
		if err := json.Unmarshal(raw, &out); err != nil {
			t.Fatalf("result: %v", err)
		}
		if len(out.Results) != 1 || out.Results[0].Properties["name"] != "ada" {
			t.Fatalf("unexpected result: %s", raw)
		}
	}
	want := `MATCH person WHERE name == "ada" RETURN *`
	if got := aikoqlQueries(log); len(got) != 2 || got[0] != want || got[1] != want {
		t.Fatalf("substituted queries: %q", got)
	}
	if err := ps.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, err := ps.Execute(ctx, map[string]any{"who": "ada"}); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("execute after close must be INVALID_ARGUMENT, got %v", err)
	}
	if _, err := bound.Execute(ctx); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("bound statement must ride the closed statement, got %v", err)
	}
}

func TestPreparedParameterTypeMismatch(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	ps, err := c.Prepare(context.Background(), "MATCH person WHERE age == :age RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	// Containers are not bindable scalars.
	_, err = ps.Execute(context.Background(), map[string]any{"age": []any{1, 2}})
	if mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("container binding must be INVALID_ARGUMENT, got %v", err)
	}
}

func TestPreparedWrongParameterCount(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	ps, err := c.Prepare(context.Background(), "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if _, err := ps.Execute(context.Background(), nil); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("missing binding must be INVALID_ARGUMENT, got %v", err)
	}
	if _, err := ps.Execute(context.Background(),
		map[string]any{"who": "ada", "extra": 1}); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("unknown binding must be INVALID_ARGUMENT, got %v", err)
	}
}

func TestPreparedInvalidStatement(t *testing.T) {
	// Client-side arm: an empty query is refused at prepare.
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if _, err := c.Prepare(context.Background(), "   "); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("empty query must be INVALID_ARGUMENT at prepare, got %v", err)
	}
	// Server-side arm: a syntactically bad query compiles at the server on
	// execute and the server's error surfaces as-is.
	addr2 := fakeServer(t, invalidQueryResponder(t))
	c2, err := Dial(context.Background(), addr2)
	if err != nil {
		t.Fatalf("Dial 2: %v", err)
	}
	defer c2.Close()
	ps, err := c2.Prepare(context.Background(), "MATCH WHERE garbage")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if _, err := ps.Execute(context.Background(), nil); mcpErrorCode(t, err) != "INVALID_QUERY" {
		t.Fatalf("server compile failure must surface as INVALID_QUERY, got %v", err)
	}
}

func TestPreparedSchemaInvalidationRecompiles(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	ctx := context.Background()

	ps, err := c.Prepare(ctx, "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if _, err := ps.Execute(ctx, map[string]any{"who": "ada"}); err != nil {
		t.Fatalf("Execute 1: %v", err)
	}
	// The schema changes under the statement (a new type lands)...
	if _, err := c.Remember(ctx, RememberParams{
		TypeName: "pet", Properties: map[string]any{"name": "rex"},
	}); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	// ... and the next execute re-compiles: there is no cached plan to
	// invalidate.
	if _, err := ps.Execute(ctx, map[string]any{"who": "ada"}); err != nil {
		t.Fatalf("Execute 2: %v", err)
	}
	if got := len(aikoqlQueries(log)); got != 2 {
		t.Fatalf("expected 2 compiled executes, got %d", got)
	}
}

func TestPreparedTransactionInteraction(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server integration test skipped")
	}
	addr, stop := startRealServer(t, bin)
	defer stop()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-prepared-integration", "0.0.0-test"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize: %v", err)
	}

	ps, err := c.Prepare(ctx, "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	raw, err := ps.Execute(ctx, map[string]any{"who": "ada"})
	if err != nil {
		t.Fatalf("Execute (empty): %v", err)
	}
	if bytes.Contains(raw, []byte("ada")) {
		t.Fatalf("nothing should match before the write: %s", raw)
	}
	tx, err := c.Begin(ctx)
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if err := tx.Execute(ctx, StagedOp{Action: "create", TypeName: "person",
		Properties: map[string]any{"name": "ada"}}); err != nil {
		t.Fatalf("stage: %v", err)
	}
	if _, err := tx.Commit(ctx); err != nil {
		t.Fatalf("Commit: %v", err)
	}
	raw, err = ps.Execute(ctx, map[string]any{"who": "ada"})
	if err != nil {
		t.Fatalf("Execute (after commit): %v", err)
	}
	if !bytes.Contains(raw, []byte("ada")) {
		t.Fatalf("a prepared read must see the committed row: %s", raw)
	}
}

func TestPreparedConcurrentUse(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	ps, err := c.Prepare(context.Background(), "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	bound := ps.Bind(map[string]any{"who": "ada"})
	// Four goroutines execute the same bound statement concurrently; the
	// client serializes the wire (one connection, one mutex).
	var wg sync.WaitGroup
	errs := make(chan error, 20)
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 5; j++ {
				raw, err := bound.Execute(context.Background())
				if err != nil {
					errs <- err
					return
				}
				if !bytes.Contains(raw, []byte("ada")) {
					errs <- errors.New("result missing ada")
					return
				}
			}
		}()
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatalf("concurrent execute: %v", err)
	}
	if got := len(aikoqlQueries(log)); got != 20 {
		t.Fatalf("expected 20 compiled executes, got %d", got)
	}
}

func TestPreparedCloseThenExecuteRefused(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, preparedResponder(t, &log))
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	ctx := context.Background()

	ps, err := c.Prepare(ctx, "MATCH person RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if err := ps.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, err := ps.Execute(ctx, nil); mcpErrorCode(t, err) != "INVALID_ARGUMENT" {
		t.Fatalf("execute after close must be INVALID_ARGUMENT, got %v", err)
	}
	// close is per-statement: a fresh prepare works on the same client.
	ps2, err := c.Prepare(ctx, "MATCH person RETURN *")
	if err != nil {
		t.Fatalf("Prepare 2: %v", err)
	}
	if _, err := ps2.Execute(ctx, nil); err != nil {
		t.Fatalf("Execute on a fresh prepare: %v", err)
	}
}

func TestPreparedRecompilesAfterServerRestart(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server integration test skipped")
	}
	probe, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("probe listen: %v", err)
	}
	port := probe.Addr().(*net.TCPAddr).Port
	if err := probe.Close(); err != nil {
		t.Fatalf("probe close: %v", err)
	}
	addr := net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	dbDir := filepath.Join(t.TempDir(), "kb")

	stop1 := startRealServerOn(t, bin, addr, dbDir)
	defer stop1()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-prepared-integration", "0.0.0-test"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize: %v", err)
	}
	ps, err := c.Prepare(ctx, "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if _, err := c.Remember(ctx, RememberParams{
		TypeName: "person", Properties: map[string]any{"name": "ada"},
	}); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	raw, err := ps.Execute(ctx, map[string]any{"who": "ada"})
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !bytes.Contains(raw, []byte("ada")) {
		t.Fatalf("the row must match before the restart: %s", raw)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	stop1() // the server dies
	stop2 := startRealServerOn(t, bin, addr, dbDir)
	defer stop2()

	// The pool reconnect leg (TestPoolReconnectsAfterServerRestart) blocks
	// the borrow and returns once the server is back; the prepared
	// statement held no server-side plan, so a fresh prepare on the new
	// connection compiles it afresh against the restarted server.
	factory := func(ctx context.Context) (*Client, error) {
		c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-prepared-integration", "0.0.0-test"))
		if err != nil {
			return nil, err
		}
		if err := c.Initialize(ctx); err != nil {
			c.Close()
			return nil, err
		}
		return c, nil
	}
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 1,
		AcquireTimeout: 10 * time.Second, HealthCheckInterval: time.Nanosecond})
	defer p.Close()
	pc, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire after restart: %v", err)
	}
	ps2, err := pc.Client().Prepare(ctx, "MATCH person WHERE name == :who RETURN *")
	if err != nil {
		t.Fatalf("Prepare after restart: %v", err)
	}
	raw, err = ps2.Execute(ctx, map[string]any{"who": "ada"})
	if err != nil {
		t.Fatalf("Execute after restart: %v", err)
	}
	if !bytes.Contains(raw, []byte("ada")) {
		t.Fatalf("the db must survive the restart: %s", raw)
	}
	if err := pc.Release(); err != nil {
		t.Fatalf("Release: %v", err)
	}
}
