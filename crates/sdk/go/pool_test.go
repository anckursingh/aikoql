package aikoql

// D-09: the §3.4 connection pool and the §21 behavioral spec. The
// scripted-server legs pin the pool mechanics (exhaustion →
// RESOURCE_EXHAUSTED, transaction pinning + session reset on release,
// reuse after a cancelled call, min-idle fill); the real-server leg
// (skipped without AIKOQL_MCP_BIN) pins reconnect and auth reset across
// a server restart. The factory is the seam: each call must return a
// fully established session (dialed + initialized) for one connection.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

// poolResponder answers every tool a pooled connection can issue and logs
// the call through log.
func poolResponder(t *testing.T, log *[]txnCall) func(t *testing.T, req rpcFrame) []rpcFrame {
	t.Helper()
	return func(t *testing.T, req rpcFrame) []rpcFrame {
		var p txnCall
		if err := json.Unmarshal(req.Params, &p); err != nil {
			t.Fatalf("request params: %v", err)
		}
		*log = append(*log, p)
		var data any
		switch p.Name {
		case "health":
			data = map[string]any{"status": "ok"}
		case "aikoql":
			data = map[string]any{"results": []any{}}
		case "txn_begin":
			data = map[string]any{"txn_id": p.Args["txn_id"], "snapshot_ts": 1000}
		case "txn_stage":
			data = map[string]any{"staged": 1}
		case "txn_rollback":
			data = map[string]any{"rolled_back": true}
		default:
			t.Fatalf("unexpected tool %q", p.Name)
		}
		return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
			Result: toolResult(map[string]any{"ok": true, "data": data})}}
	}
}

// poolFactory returns a factory that dials a fresh scripted server per
// call and a dial counter.
func poolFactory(t *testing.T, respond func(t *testing.T, req rpcFrame) []rpcFrame) (factory func(ctx context.Context) (*Client, error), dials *int) {
	t.Helper()
	n := 0
	return func(ctx context.Context) (*Client, error) {
		addr := fakeServer(t, respond)
		n++
		return Dial(ctx, addr)
	}, &n
}

func TestPoolExhaustionWaitsThenResourceExhausted(t *testing.T) {
	var log []txnCall
	factory, dials := poolFactory(t, poolResponder(t, &log))
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 2, AcquireTimeout: 200 * time.Millisecond})
	defer p.Close()
	ctx := context.Background()

	a, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire a: %v", err)
	}
	b, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire b: %v", err)
	}
	if *dials != 2 {
		t.Fatalf("two acquires must dial twice, got %d", *dials)
	}
	start := time.Now()
	_, err = p.Acquire(ctx)
	var me *McpError
	if !errors.As(err, &me) || me.Code != "RESOURCE_EXHAUSTED" || !me.Retryable {
		t.Fatalf("third acquire must be RESOURCE_EXHAUSTED, got %v", err)
	}
	if elapsed := time.Since(start); elapsed < 190*time.Millisecond {
		t.Fatalf("acquire must wait for the timeout before failing, waited %v", elapsed)
	}
	if *dials != 2 {
		t.Fatalf("an exhausted pool must not dial, got %d dials", *dials)
	}
	// A freed connection is handed to a waiter without a redial.
	got := make(chan *PooledConn, 1)
	go func() {
		pc, err := p.Acquire(ctx)
		if err != nil {
			t.Errorf("waiter Acquire: %v", err)
			got <- nil
			return
		}
		got <- pc
	}()
	time.Sleep(50 * time.Millisecond)
	if err := a.Release(); err != nil {
		t.Fatalf("Release a: %v", err)
	}
	select {
	case c := <-got:
		if c == nil {
			t.Fatal("waiter acquire failed")
		}
		if *dials != 2 {
			t.Fatalf("handoff must not dial, got %d dials", *dials)
		}
		if err := c.Release(); err != nil {
			t.Fatalf("Release c: %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("waiter never received the freed connection")
	}
	if err := b.Release(); err != nil {
		t.Fatalf("Release b: %v", err)
	}
}

func TestPoolReleaseResetsOpenTransaction(t *testing.T) {
	var log []txnCall
	factory, dials := poolFactory(t, poolResponder(t, &log))
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 1})
	defer p.Close()
	ctx := context.Background()

	pc, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire: %v", err)
	}
	tx, err := pc.Begin(ctx, "abc")
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if err := tx.Execute(ctx, StagedOp{Action: "create", TypeName: "person",
		Properties: map[string]any{"name": "ada"}}); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if err := pc.Release(); err != nil {
		t.Fatalf("Release: %v", err)
	}
	// Session reset: the open transaction was rolled back on release.
	rolledBack := false
	for _, call := range log {
		if call.Name == "txn_rollback" && call.Args["txn_id"] == "abc" {
			rolledBack = true
		}
	}
	if !rolledBack {
		t.Fatalf("release must roll back the open transaction, log: %+v", log)
	}
	// The next borrower must NOT inherit the transaction (§21).
	pc2, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire 2: %v", err)
	}
	if *dials != 1 {
		t.Fatalf("the connection must be reused, got %d dials", *dials)
	}
	tx2, err := pc2.Begin(ctx)
	if err != nil {
		t.Fatalf("Begin 2: %v", err)
	}
	if tx2.ID == "abc" {
		t.Fatalf("next borrower must not inherit the transaction")
	}
	if len(tx2.ID) != 32 {
		t.Fatalf("fresh transaction id expected 32 hex chars, got %q", tx2.ID)
	}
	if err := pc2.Release(); err != nil {
		t.Fatalf("Release 2: %v", err)
	}
}

func TestPoolConnectionReusableAfterCancelledCall(t *testing.T) {
	var log []txnCall
	factory, dials := poolFactory(t, poolResponder(t, &log))
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 1})
	defer p.Close()

	pc, err := p.Acquire(context.Background())
	if err != nil {
		t.Fatalf("Acquire: %v", err)
	}
	cctx, cancel := context.WithCancel(context.Background())
	cancel() // a cancelled context bounds the call
	if _, err := pc.Client().Aikoql(cctx, "MATCH person RETURN *", ""); err == nil {
		t.Fatal("a call with a cancelled context must fail")
	}
	if err := pc.Release(); err != nil {
		t.Fatalf("Release: %v", err)
	}
	// The connection must be reusable (§21): same conn, no redial.
	pc2, err := p.Acquire(context.Background())
	if err != nil {
		t.Fatalf("Acquire 2: %v", err)
	}
	if *dials != 1 {
		t.Fatalf("the connection must be reused, got %d dials", *dials)
	}
	if _, err := pc2.Client().Aikoql(context.Background(), "MATCH person RETURN *", ""); err != nil {
		t.Fatalf("the connection must be reusable after a cancelled call: %v", err)
	}
	if err := pc2.Release(); err != nil {
		t.Fatalf("Release 2: %v", err)
	}
}

func TestPoolFillMinIdle(t *testing.T) {
	var log []txnCall
	factory, dials := poolFactory(t, poolResponder(t, &log))
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 3, MinIdle: 2})
	defer p.Close()

	if err := p.FillMinIdle(context.Background()); err != nil {
		t.Fatalf("FillMinIdle: %v", err)
	}
	if s := p.Stats(); s.Idle != 2 || s.Total != 2 {
		t.Fatalf("stats after fill: %+v", s)
	}
	if *dials != 2 {
		t.Fatalf("fill must dial twice, got %d", *dials)
	}
}

// startRealServerOn spawns the binary on a fixed addr with a fixed db dir
// and waits for readiness, so a test can stop and respawn on the same
// addr over the same data.
func startRealServerOn(t *testing.T, bin, addr, dbDir string) (stop func()) {
	t.Helper()
	var stderr bytes.Buffer
	cmd := exec.Command(bin, "serve",
		"--listen", addr,
		"--tcp-token", "s3cret:acme:admin",
		dbDir)
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("spawn %s: %v", bin, err)
	}
	var lastErr error
	for i := 0; i < 100; i++ {
		conn, err := net.DialTimeout("tcp", addr, time.Second)
		if err == nil {
			if cerr := conn.Close(); cerr != nil {
				t.Fatalf("readiness conn close: %v", cerr)
			}
			return func() {
				if cmd.Process != nil {
					if err := cmd.Process.Kill(); err != nil {
						t.Logf("kill mcp server: %v", err)
					}
				}
				if err := cmd.Wait(); err != nil {
					t.Logf("wait mcp server: %v", err)
				}
			}
		}
		lastErr = err
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("server never came up on %s: %v\nstderr:\n%s", addr, lastErr, stderr.String())
	return nil
}

func TestPoolReconnectsAfterServerRestart(t *testing.T) {
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
	// The factory re-establishes the full session: dial + initialize with
	// the token (the auth reset on every reconnect).
	factory := func(ctx context.Context) (*Client, error) {
		c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-pool-integration", "0.0.0-test"))
		if err != nil {
			return nil, err
		}
		if err := c.Initialize(ctx); err != nil {
			c.Close()
			return nil, err
		}
		return c, nil
	}
	// A near-zero health-check interval: every borrow pings, so a dead
	// connection is caught and replaced at checkout.
	p := NewPool(PoolConfig{Factory: factory, MaxConns: 1,
		AcquireTimeout: 10 * time.Second, HealthCheckInterval: time.Nanosecond})
	defer p.Close()

	pc, err := p.Acquire(ctx)
	if err != nil {
		t.Fatalf("Acquire: %v", err)
	}
	if _, err := pc.Client().Remember(ctx, RememberParams{
		TypeName:   "person",
		Properties: map[string]any{"name": "ada"},
	}); err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if err := pc.Release(); err != nil {
		t.Fatalf("Release: %v", err)
	}
	stop1() // the server dies

	// Borrow while the server is down: the pool keeps retrying the dial.
	type result struct {
		pc  *PooledConn
		err error
	}
	got := make(chan result, 1)
	go func() {
		pc2, err := p.Acquire(ctx)
		got <- result{pc2, err}
	}()
	select {
	case r := <-got:
		t.Fatalf("acquire must wait for the server to return, got %v", r.err)
	case <-time.After(300 * time.Millisecond):
	}
	stop2 := startRealServerOn(t, bin, addr, dbDir) // the server returns
	defer stop2()
	select {
	case r := <-got:
		if r.err != nil {
			t.Fatalf("Acquire after restart: %v", r.err)
		}
		raw, err := r.pc.Client().Aikoql(ctx, `MATCH person WHERE name == "ada" RETURN *`, "")
		if err != nil {
			t.Fatalf("query after reconnect: %v", err)
		}
		if !bytes.Contains(raw, []byte("ada")) {
			t.Fatalf("the db must survive the restart: %s", raw)
		}
		if err := r.pc.Release(); err != nil {
			t.Fatalf("Release after restart: %v", err)
		}
	case <-time.After(15 * time.Second):
		t.Fatal("acquire never completed after the restart")
	}
}
