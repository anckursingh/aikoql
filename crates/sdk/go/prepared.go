package aikoql

// The §3.6 prepared statement: prepare/bind/execute/close. The initial
// implementation compiles the AikoQL query client-side on every execute —
// the abstraction precedes a native prepare protocol, so the statement
// holds no server-side plan (nothing to invalidate, nothing lost across a
// server restart). Bind validates the binding and sends the
// named parameters to the server, which binds them as data.

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"strings"
)

var preparedPlaceholder = regexp.MustCompile(`:([a-zA-Z_][a-zA-Z0-9_]*)`)

// PreparedStatement is a validated query with its :name placeholders
// extracted.
type PreparedStatement struct {
	c      *Client
	query  string
	params []string
	closed bool
}

// BoundStatement is a prepared statement with one parameter binding;
// re-executable.
type BoundStatement struct {
	ps     *PreparedStatement
	params map[string]any
}

// Prepare validates the query client-side and extracts its placeholders.
// The ctx is part of the stable surface for the future native protocol;
// no wire call happens yet.
func (c *Client) Prepare(ctx context.Context, query string) (*PreparedStatement, error) {
	if strings.TrimSpace(query) == "" {
		return nil, &McpError{Code: "INVALID_ARGUMENT",
			Message:    "prepared statement query is empty",
			Suggestion: "Pass a non-empty AikoQL query."}
	}
	ps := &PreparedStatement{c: c, query: query}
	seen := map[string]bool{}
	for _, m := range preparedPlaceholder.FindAllStringSubmatch(query, -1) {
		if !seen[m[1]] {
			seen[m[1]] = true
			ps.params = append(ps.params, m[1])
		}
	}
	return ps, nil
}

// Bind binds the placeholders; the statement stays reusable.
func (ps *PreparedStatement) Bind(params map[string]any) *BoundStatement {
	return &BoundStatement{ps: ps, params: params}
}

// Execute is bind-and-execute in one call.
func (ps *PreparedStatement) Execute(ctx context.Context, params map[string]any) (json.RawMessage, error) {
	return ps.Bind(params).Execute(ctx)
}

// Close closes the statement; further executes are refused. There is no
// server-side handle yet, so this only marks the local state.
func (ps *PreparedStatement) Close() error {
	ps.closed = true
	return nil
}

func (ps *PreparedStatement) guard() error {
	if ps.closed {
		return &McpError{Code: "INVALID_ARGUMENT",
			Message:    "prepared statement is closed",
			Suggestion: "Prepare the statement again."}
	}
	return nil
}

// Execute validates the binding and runs the query; the server binds the
// named parameters.
func (b *BoundStatement) Execute(ctx context.Context) (json.RawMessage, error) {
	if err := b.ps.guard(); err != nil {
		return nil, err
	}
	if err := check(b.ps.params, b.params); err != nil {
		return nil, err
	}
	return b.ps.c.AikoqlParams(ctx, b.ps.query, "", b.params)
}

// check validates the binding: exact placeholder set, scalar values only.
func check(params []string, bound map[string]any) error {
	if len(bound) != len(params) {
		return &McpError{Code: "INVALID_ARGUMENT",
			Message: fmt.Sprintf("bound %d parameters, the statement has %d (%s)",
				len(bound), len(params), strings.Join(params, ", ")),
			Suggestion: "Bind exactly the statement's placeholders."}
	}
	for _, name := range params {
		value, ok := bound[name]
		if !ok {
			return &McpError{Code: "INVALID_ARGUMENT",
				Message:    fmt.Sprintf("parameter :%s is not bound", name),
				Suggestion: "Bind exactly the statement's placeholders."}
		}
		switch value.(type) {
		case string, float64, int, int64, bool, nil, json.Number:
		default:
			return &McpError{Code: "INVALID_ARGUMENT",
				Message:    fmt.Sprintf("parameter :%s must be a scalar, got %T", name, value),
				Suggestion: "Bind scalars (string, number, bool, null) only."}
		}
	}
	return nil
}
