package aikoql

// Tx and its companions implement the §3.5 transaction handle: the
// txn_id is a first-class field here — it never leaks as a bare tool
// argument that a caller threads between call sites. The four txn_* tools
// are reachable only through Tx methods.

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
)

// Tx is a staged write handle: Begin on the Client, Execute stages ops,
// Commit or Rollback closes it. A closed handle refuses further use with
// INVALID_ARGUMENT.
type Tx struct {
	c    *Client
	ID   string
	done bool
}

// StagedOp is one staged write (the txn_stage op shape).
type StagedOp struct {
	Action     string         `json:"action"`
	TypeName   string         `json:"type_name,omitempty"`
	KOID       string         `json:"koid,omitempty"`
	Properties map[string]any `json:"properties,omitempty"`
}

// CommitResult is the txn_commit outcome: the staged write results and
// whether this commit was a retry of an already-committed txn_id.
type CommitResult struct {
	Results []json.RawMessage `json:"results"`
	Deduped bool              `json:"deduped"`
}

// Begin opens a transaction. With no id a random 32-hex id is generated;
// passing one retries the same begin idempotently (P5-M20).
func (c *Client) Begin(ctx context.Context, txnID ...string) (*Tx, error) {
	var id string
	if len(txnID) > 0 {
		id = txnID[0]
	} else {
		b := make([]byte, 16)
		if _, err := rand.Read(b); err != nil {
			return nil, fmt.Errorf("aikoql: txn id: %w", err)
		}
		id = hex.EncodeToString(b)
	}
	if _, err := c.CallTool(ctx, "txn_begin", map[string]any{"txn_id": id}); err != nil {
		return nil, err
	}
	return &Tx{c: c, ID: id}, nil
}

// Execute stages one write.
func (t *Tx) Execute(ctx context.Context, op StagedOp) error {
	_, err := t.step(ctx, "txn_stage", map[string]any{"txn_id": t.ID, "op": op})
	return err
}

// Commit applies the staged writes and closes the handle. A commit that
// errors leaves the handle open: the server dedupes by txn_id, so the
// caller may retry Commit or Begin with the same id (P5-M20).
func (t *Tx) Commit(ctx context.Context) (*CommitResult, error) {
	raw, err := t.step(ctx, "txn_commit", map[string]any{"txn_id": t.ID})
	if err != nil {
		return nil, err
	}
	var res CommitResult
	if err := json.Unmarshal(raw, &res); err != nil {
		return nil, fmt.Errorf("aikoql: txn_commit payload: %w", err)
	}
	return &res, nil
}

// Rollback discards the staged writes and closes the handle.
func (t *Tx) Rollback(ctx context.Context) error {
	_, err := t.step(ctx, "txn_rollback", map[string]any{"txn_id": t.ID})
	return err
}

func (t *Tx) step(ctx context.Context, name string, args map[string]any) (json.RawMessage, error) {
	if t.done {
		return nil, &McpError{
			Code:       "INVALID_ARGUMENT",
			Message:    fmt.Sprintf("transaction %s is closed", t.ID),
			Suggestion: "Begin a new transaction.",
		}
	}
	raw, err := t.c.CallTool(ctx, name, args)
	if err != nil {
		return nil, err
	}
	if name == "txn_commit" || name == "txn_rollback" {
		t.done = true
	}
	return raw, nil
}
