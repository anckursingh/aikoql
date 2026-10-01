package aikoql

// D-08: the §3.5 transaction handle — Begin / Tx.Execute / Tx.Commit /
// Tx.Rollback. The txn_id is a first-class field on Tx, never a bare tool
// argument the caller threads between call sites. The scripted-server legs
// pin the SDK shape and the done-guard; the real-server leg (skipped
// without AIKOQL_MCP_BIN) pins the semantics against the actual database.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"regexp"
	"testing"
	"time"
)

type txnCall struct {
	Name string         `json:"name"`
	Args map[string]any `json:"arguments"`
}

func txnResponder(t *testing.T, log *[]txnCall) func(t *testing.T, req rpcFrame) []rpcFrame {
	t.Helper()
	return func(t *testing.T, req rpcFrame) []rpcFrame {
		var p txnCall
		if err := json.Unmarshal(req.Params, &p); err != nil {
			t.Fatalf("request params: %v", err)
		}
		*log = append(*log, p)
		var data any
		switch p.Name {
		case "txn_begin":
			data = map[string]any{"txn_id": p.Args["txn_id"], "snapshot_ts": 1000}
		case "txn_stage":
			data = map[string]any{"staged": 1}
		case "txn_commit":
			data = map[string]any{
				"results": []any{map[string]any{"action": "create", "koid": "k1"}},
				"deduped": false,
			}
		case "txn_rollback":
			data = map[string]any{"rolled_back": true}
		default:
			t.Fatalf("unexpected tool %q", p.Name)
		}
		return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
			Result: toolResult(map[string]any{"ok": true, "data": data})}}
	}
}

func TestTxScriptedCommitRoundTrip(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, txnResponder(t, &log))
	c := mustDial(t, addr)
	ctx := context.Background()

	tx, err := c.Begin(ctx, "abc")
	if err != nil || tx.ID != "abc" {
		t.Fatalf("Begin: %v, tx=%+v", err, tx)
	}
	if err := tx.Execute(ctx, StagedOp{
		Action:     "create",
		TypeName:   "person",
		Properties: map[string]any{"name": "Ada"},
	}); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	res, err := tx.Commit(ctx)
	if err != nil || res.Deduped || len(res.Results) != 1 {
		t.Fatalf("Commit: %v, res=%+v", err, res)
	}
	// The handle is closed by commit — further use is INVALID_ARGUMENT.
	err = tx.Execute(ctx, StagedOp{Action: "create"})
	var me *McpError
	if !errors.As(err, &me) || me.Code != "INVALID_ARGUMENT" {
		t.Fatalf("use after commit must be INVALID_ARGUMENT, got %v", err)
	}
	// The wire calls carry the handle id in the tool arguments.
	if len(log) != 3 || log[0].Name != "txn_begin" || log[1].Name != "txn_stage" ||
		log[2].Name != "txn_commit" {
		t.Fatalf("call log wrong: %+v", log)
	}
	if log[1].Args["txn_id"] != "abc" {
		t.Fatalf("stage must carry the handle id, got %v", log[1].Args["txn_id"])
	}
}

func TestTxScriptedRollbackRoundTrip(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, txnResponder(t, &log))
	c := mustDial(t, addr)
	ctx := context.Background()

	tx, err := c.Begin(ctx, "abc")
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if err := tx.Rollback(ctx); err != nil {
		t.Fatalf("Rollback: %v", err)
	}
	err = tx.Commit(ctx)
	var me *McpError
	if !errors.As(err, &me) || me.Code != "INVALID_ARGUMENT" {
		t.Fatalf("commit after rollback must be INVALID_ARGUMENT, got %v", err)
	}
	if len(log) != 2 || log[1].Name != "txn_rollback" {
		t.Fatalf("call log wrong: %+v", log)
	}
}

func TestTxGeneratedID(t *testing.T) {
	var log []txnCall
	addr := fakeServer(t, txnResponder(t, &log))
	c := mustDial(t, addr)
	ctx := context.Background()

	tx, err := c.Begin(ctx)
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if !regexp.MustCompile(`^[0-9a-f]{32}$`).MatchString(tx.ID) {
		t.Fatalf("generated txn id %q is not 32 hex chars", tx.ID)
	}
	if log[0].Args["txn_id"] != tx.ID {
		t.Fatalf("begin must carry the generated id, got %v", log[0].Args["txn_id"])
	}
}

func TestTxRealServerCommitVisibleAndRollbackGone(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server integration test skipped")
	}
	addr, stop := startRealServer(t, bin)
	defer stop()
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-tx-integration", "0.0.0-test"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize: %v", err)
	}

	tx, err := c.Begin(ctx)
	if err != nil {
		t.Fatalf("Begin: %v", err)
	}
	if err := tx.Execute(ctx, StagedOp{
		Action:     "create",
		TypeName:   "person",
		Properties: map[string]any{"name": "ada"},
	}); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if _, err := tx.Commit(ctx); err != nil {
		t.Fatalf("Commit: %v", err)
	}
	raw, err := c.Aikoql(ctx, "MATCH person WHERE name == \"ada\" RETURN *", "")
	if err != nil {
		t.Fatalf("Aikoql after commit: %v", err)
	}
	if !bytes.Contains(raw, []byte("ada")) {
		t.Fatalf("committed stage must be visible: %s", raw)
	}

	tx2, err := c.Begin(ctx)
	if err != nil {
		t.Fatalf("Begin 2: %v", err)
	}
	if err := tx2.Execute(ctx, StagedOp{
		Action:     "create",
		TypeName:   "person",
		Properties: map[string]any{"name": "ghost"},
	}); err != nil {
		t.Fatalf("Execute 2: %v", err)
	}
	if err := tx2.Rollback(ctx); err != nil {
		t.Fatalf("Rollback 2: %v", err)
	}
	raw2, err := c.Aikoql(ctx, "MATCH person WHERE name == \"ghost\" RETURN *", "")
	if err != nil {
		t.Fatalf("Aikoql after rollback: %v", err)
	}
	if bytes.Contains(raw2, []byte("ghost")) {
		t.Fatalf("rolled back stage must be gone: %s", raw2)
	}
}
