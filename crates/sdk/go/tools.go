package aikoql

// Tool wrappers — the typed DB surface over the MCP tools. Every call
// takes ctx first; subject is omitted from the wire when empty (the
// server assigns identity). The tool registry of the server is the
// schema source: crates/services/api/mcp/src/tool_registry.rs.

import (
	"context"
	"encoding/json"
	"fmt"
)

// RememberParams is one knowledge-object commit (create or new version).
type RememberParams struct {
	Subject         string         `json:"subject,omitempty"`
	TypeName        string         `json:"type_name"`
	KOID            string         `json:"koid,omitempty"`
	Properties      map[string]any `json:"properties,omitempty"`
	Note            string         `json:"note,omitempty"`
	IdempotencyKey  string         `json:"idempotency_key,omitempty"`
	Embed           bool           `json:"embed,omitempty"`
	ExpectedVersion *uint64        `json:"expected_version,omitempty"`
	RetentionMs     *int64         `json:"retention_ms,omitempty"`
	Semantic        map[string]any `json:"semantic,omitempty"`
}

// Remembered is the server's answer to remember.
type Remembered struct {
	KOID     string `json:"koid"`
	Version  uint64 `json:"version"`
	CommitTS uint64 `json:"commit_ts"`
}

// Remember commits a knowledge object (or a new version of one).
func (c *Client) Remember(ctx context.Context, p RememberParams) (*Remembered, error) {
	raw, err := c.CallTool(ctx, "remember", p)
	if err != nil {
		return nil, err
	}
	var r Remembered
	if err := json.Unmarshal(raw, &r); err != nil {
		return nil, wrapTool("remember", err)
	}
	return &r, nil
}

// KnowledgeObject is a fetched KO.
type KnowledgeObject struct {
	KOID       string         `json:"koid"`
	Version    uint64         `json:"version"`
	State      string         `json:"state,omitempty"`
	Properties map[string]any `json:"properties"`
}

// Get fetches a knowledge object by KOID.
func (c *Client) Get(ctx context.Context, koid, subject string) (*KnowledgeObject, error) {
	args := map[string]any{"koid": koid}
	if subject != "" {
		args["subject"] = subject
	}
	raw, err := c.CallTool(ctx, "get", args)
	if err != nil {
		return nil, err
	}
	var ko KnowledgeObject
	if err := json.Unmarshal(raw, &ko); err != nil {
		return nil, wrapTool("get", err)
	}
	return &ko, nil
}

// Forget tombstones ("tombstone") or legally erases ("erase") a knowledge
// object, audit-preserving.
func (c *Client) Forget(ctx context.Context, koid, mode, subject string) (json.RawMessage, error) {
	args := map[string]any{"koid": koid, "mode": mode}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "forget", args)
}

// FindSimilarParams is a hybrid recall query (vector + text + filters,
// RRF/weighted fusion).
type FindSimilarParams struct {
	Subject            string    `json:"subject,omitempty"`
	Text               string    `json:"text,omitempty"`
	Vector             []float64 `json:"vector,omitempty"`
	TypeName           string    `json:"type_name,omitempty"`
	K                  int       `json:"k,omitempty"`
	Fusion             string    `json:"fusion,omitempty"`
	EmbeddingModel     string    `json:"embedding_model,omitempty"`
	WaitForFreshnessMs *int64    `json:"wait_for_freshness_ms,omitempty"`
}

// ScoredKO is one recall hit.
type ScoredKO struct {
	KOID     string  `json:"koid"`
	Score    float64 `json:"score"`
	TypeName string  `json:"type_name"`
}

// FindSimilar runs hybrid recall and returns the scored hits.
func (c *Client) FindSimilar(ctx context.Context, p FindSimilarParams) ([]ScoredKO, error) {
	raw, err := c.CallTool(ctx, "find_similar", p)
	if err != nil {
		return nil, err
	}
	var res struct {
		Results []ScoredKO `json:"results"`
	}
	if err := json.Unmarshal(raw, &res); err != nil {
		return nil, wrapTool("find_similar", err)
	}
	return res.Results, nil
}

// Aikoql runs an AikoQL query ("MATCH person WHERE name == \"ada\" RETURN *")
// and returns the raw result rows for the caller to unmarshal.
func (c *Client) Aikoql(ctx context.Context, query, subject string) (json.RawMessage, error) {
	args := map[string]any{"query": query}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "aikoql", args)
}

// AikoqlStream runs a streaming query; yields the response frame (the
// first data chunk), then each notify chunk until its done flag. The
// Client must not be shared while the stream is open.
func (c *Client) AikoqlStream(ctx context.Context, query, subject string, yieldFn func(chunk json.RawMessage) error) error {
	params := map[string]any{"query": query}
	if subject != "" {
		params["subject"] = subject
	}
	return c.stream(ctx, "aikoql/stream", params, yieldFn)
}

// Relate links two knowledge objects.
func (c *Client) Relate(ctx context.Context, fromKOID, toKOID, relType, subject string) (json.RawMessage, error) {
	args := map[string]any{"from": fromKOID, "to": toKOID, "rel_type": relType}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "relate", args)
}

// Traverse walks the relationship graph from a KOID.
func (c *Client) Traverse(ctx context.Context, koid, relType, subject string, depth int) (json.RawMessage, error) {
	args := map[string]any{"koid": koid, "depth": depth}
	if relType != "" {
		args["rel_type"] = relType
	}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "traverse", args)
}

// Batch sends several operations in one call (the server's batch shape).
func (c *Client) Batch(ctx context.Context, operations []map[string]any) (json.RawMessage, error) {
	return c.CallTool(ctx, "batch", map[string]any{"operations": operations})
}

// Health returns the server health payload.
func (c *Client) Health(ctx context.Context) (json.RawMessage, error) {
	return c.CallTool(ctx, "health", nil)
}

// DiscoverSchema returns the schema-discovery payload.
func (c *Client) DiscoverSchema(ctx context.Context) (json.RawMessage, error) {
	return c.CallTool(ctx, "discover_schema", nil)
}

// Decide records a decision on a KO.
func (c *Client) Decide(ctx context.Context, koid, decision, rationale string, confidence float64) (json.RawMessage, error) {
	return c.CallTool(ctx, "decide", map[string]any{
		"koid": koid, "decision": decision,
		"rationale": rationale, "confidence": confidence,
	})
}

// AgentMemory reads/writes a per-agent key (value == nil reads).
func (c *Client) AgentMemory(ctx context.Context, agentID, key string, value any, ttl int) (json.RawMessage, error) {
	args := map[string]any{"agent_id": agentID, "ttl": ttl}
	if key != "" {
		args["key"] = key
	}
	if value != nil {
		args["value"] = value
	}
	return c.CallTool(ctx, "agent_memory", args)
}

// Metrics is the server's metrics payload.
type Metrics struct {
	JournalSeq    uint64         `json:"journal_seq"`
	TotalObjects  int            `json:"total_objects"`
	ActiveObjects int            `json:"active_objects"`
	UptimeSeconds float64        `json:"uptime_seconds"`
	ByLifecycle   map[string]int `json:"by_lifecycle"`
	ByType        map[string]int `json:"by_type"`
}

// Metrics returns the server's metrics.
func (c *Client) Metrics(ctx context.Context) (*Metrics, error) {
	raw, err := c.CallTool(ctx, "metrics", nil)
	if err != nil {
		return nil, err
	}
	var m Metrics
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, wrapTool("metrics", err)
	}
	return &m, nil
}

// Trace returns the full lineage of a fact (versions + events).
func (c *Client) Trace(ctx context.Context, koid, subject string) (json.RawMessage, error) {
	args := map[string]any{"koid": koid}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "trace", args)
}

// Explain returns the explanation payload for a KO version.
func (c *Client) Explain(ctx context.Context, koid, subject string, version *uint64) (json.RawMessage, error) {
	args := map[string]any{"koid": koid}
	if version != nil {
		args["version"] = *version
	}
	if subject != "" {
		args["subject"] = subject
	}
	return c.CallTool(ctx, "explain", args)
}

// wrapTool attaches the tool name to an unmarshal failure.
func wrapTool(name string, err error) error {
	return fmt.Errorf("aikoql: %s payload: %w", name, err)
}
