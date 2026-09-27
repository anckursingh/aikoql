// Package aikoql is the Go SDK for the AikoQL knowledge database. It
// speaks MCP JSON-RPC over TCP to an aikoql-mcp server — the same first-
// class surface the Python SDK and standard MCP clients use:
//
//	ctx := context.Background()
//	db, err := aikoql.Dial(ctx, "127.0.0.1:9090", aikoql.WithToken("s3cret"))
//	if err != nil { ... }
//	defer db.Close()
//	if err := db.Initialize(ctx); err != nil { ... }
//	ko, err := db.Remember(ctx, aikoql.RememberParams{
//	    TypeName: "person", Properties: map[string]any{"name": "ada"},
//	})
//	rows, err := db.Aikoql(ctx, "MATCH person RETURN *", "")
//
// A Client serializes its calls over one connection (the mutex is
// internal) — pool Clients like you would pool any DB connection, or open
// one per goroutine. Server push frames (notifications) are skipped and
// correlated by id, never by order.
//
// The wire transport is private to the Client: if a native protocol ever
// replaces MCP JSON-RPC, the tool surface above stays and only this file
// changes.
package aikoql

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"
)

// MIN_SERVER_VERSION is the oldest aikoql-mcp server this SDK will talk
// to (the ND-12 version contract, mirrored from the Python SDK; pinned
// against the workspace version by the go-sdk CI job).
const MIN_SERVER_VERSION = "0.1.19"

// McpError is a structured error from the MCP server (MRFC-0040 error
// codes): the tool-level ok/error envelope and RPC-level failures both
// carry it.
type McpError struct {
	Code       string `json:"code"`
	Message    string `json:"message"`
	Retryable  bool   `json:"retryable"`
	Suggestion string `json:"suggestion"`
}

func (e *McpError) Error() string {
	return fmt.Sprintf("[%s] %s", e.Code, e.Message)
}

// clientConfig holds the dial options.
type clientConfig struct {
	token       string
	name        string
	version     string
	dialTimeout time.Duration
}

// Option tunes Dial.
type Option func(*clientConfig)

// WithToken sends the --tcp-token credential in the initialize handshake
// (required by every TCP server since P3-M1).
func WithToken(token string) Option {
	return func(c *clientConfig) { c.token = token }
}

// WithClientInfo sets the MCP client identity advertised at initialize.
func WithClientInfo(name, version string) Option {
	return func(c *clientConfig) { c.name, c.version = name, version }
}

// Client is one MCP JSON-RPC connection to an aikoql-mcp server.
type Client struct {
	conn   net.Conn
	r      *bufio.Reader
	mu     sync.Mutex // serializes frames: one in-flight call per conn
	nextID int64
	cfg    clientConfig
}

// Dial opens a TCP connection to addr ("host:port"). The handshake is
// separate (Initialize) so a pooled Client can dial first and authenticate
// later; every aikoql-mcp TCP server requires the token.
func Dial(ctx context.Context, addr string, opts ...Option) (*Client, error) {
	cfg := clientConfig{name: "aikoql-go-sdk", version: "0.1.0", dialTimeout: 5 * time.Second}
	for _, o := range opts {
		o(&cfg)
	}
	d := net.Dialer{Timeout: cfg.dialTimeout}
	conn, err := d.DialContext(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("aikoql: dial %s: %w", addr, err)
	}
	return &Client{conn: conn, r: bufio.NewReader(conn), cfg: cfg}, nil
}

// Close closes the connection. Safe to call more than once.
func (c *Client) Close() error {
	return c.conn.Close()
}

// rpcRequest is one JSON-RPC request frame.
type rpcRequest struct {
	JSONRPC string `json:"jsonrpc"`
	ID      int64  `json:"id"`
	Method  string `json:"method"`
	Params  any    `json:"params,omitempty"`
}

// rpcResponse covers both responses (id, result/error) and pushes (no id,
// method notifications/notify, params).
type rpcResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int64           `json:"id,omitempty"`
	Method  string          `json:"method,omitempty"`
	Result  json.RawMessage `json:"result,omitempty"`
	Params  json.RawMessage `json:"params,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    json.RawMessage `json:"code"`
	Message string          `json:"message"`
}

func (e *rpcError) mcpError() *McpError {
	code := string(e.Code)
	if len(code) > 0 && code[0] == '"' { // string-encoded code: strip quotes
		code = strings.Trim(code, `"`)
	}
	if code == "" {
		code = "INTERNAL"
	}
	return &McpError{Code: code, Message: e.Message}
}

// applyDeadline projects the context deadline onto the socket so a stalled
// server cannot hang the caller past ctx.
func (c *Client) applyDeadline(ctx context.Context) error {
	if d, ok := ctx.Deadline(); ok {
		if err := c.conn.SetDeadline(d); err != nil {
			return fmt.Errorf("aikoql: set deadline: %w", err)
		}
		return nil
	}
	if err := c.conn.SetDeadline(time.Time{}); err != nil {
		return fmt.Errorf("aikoql: clear deadline: %w", err)
	}
	return nil
}

// request sends one JSON-RPC request and returns its result frame,
// skipping pushed notifications by id correlation.
func (c *Client) request(ctx context.Context, method string, params any) (json.RawMessage, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if err := c.applyDeadline(ctx); err != nil {
		return nil, err
	}
	c.nextID++
	id := c.nextID
	frame, err := json.Marshal(rpcRequest{JSONRPC: "2.0", ID: id, Method: method, Params: params})
	if err != nil {
		return nil, fmt.Errorf("aikoql: marshal %s: %w", method, err)
	}
	if _, err := c.conn.Write(append(frame, '\n')); err != nil {
		return nil, fmt.Errorf("aikoql: send %s: %w", method, err)
	}
	for {
		line, err := c.r.ReadString('\n')
		if err != nil {
			return nil, fmt.Errorf("aikoql: read %s: %w", method, err)
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(line), &resp); err != nil {
			continue // tolerate non-JSON noise frames
		}
		if resp.ID != id {
			continue // push or another call's frame
		}
		if resp.Error != nil {
			return nil, resp.Error.mcpError()
		}
		return resp.Result, nil
	}
}

// stream sends one request and yields every data chunk of the response —
// the initial result frame is consumed for its stream id, then each
// notifications/notify frame carrying that id is handed to yieldFn until
// its done flag. The mutex is held for the whole stream: do not share the
// Client across goroutines while streaming.
func (c *Client) stream(ctx context.Context, method string, params any, yieldFn func(chunk json.RawMessage) error) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if err := c.applyDeadline(ctx); err != nil {
		return err
	}
	c.nextID++
	id := c.nextID
	frame, err := json.Marshal(rpcRequest{JSONRPC: "2.0", ID: id, Method: method, Params: params})
	if err != nil {
		return fmt.Errorf("aikoql: marshal %s: %w", method, err)
	}
	if _, err := c.conn.Write(append(frame, '\n')); err != nil {
		return fmt.Errorf("aikoql: send %s: %w", method, err)
	}
	var streamID string
	for {
		line, err := c.r.ReadString('\n')
		if err != nil {
			return fmt.Errorf("aikoql: read %s: %w", method, err)
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(line), &resp); err != nil {
			continue
		}
		if resp.ID == id {
			if resp.Error != nil {
				return resp.Error.mcpError()
			}
			var head struct {
				StreamID string `json:"stream_id"`
			}
			if err := json.Unmarshal(resp.Result, &head); err != nil {
				return fmt.Errorf("aikoql: %s stream head: %w", method, err)
			}
			streamID = head.StreamID
			continue
		}
		if streamID == "" || resp.Method != "notifications/notify" {
			continue // push before the response, or an unrelated event
		}
		var p struct {
			StreamID string `json:"stream_id"`
			Done     bool   `json:"done"`
		}
		if err := json.Unmarshal(resp.Params, &p); err != nil {
			continue
		}
		if p.StreamID != streamID {
			continue
		}
		if err := yieldFn(resp.Params); err != nil {
			return fmt.Errorf("aikoql: %s consumer: %w", method, err)
		}
		if p.Done {
			return nil
		}
	}
}

// Initialize performs the MCP handshake (protocol version, client info,
// token) and enforces the ND-12 version contract: a server older than
// MIN_SERVER_VERSION fails fast with VERSION_MISMATCH.
func (c *Client) Initialize(ctx context.Context) error {
	params := map[string]any{
		"protocolVersion": "2024-11-05",
		"capabilities":    map[string]any{},
		"clientInfo":      map[string]any{"name": c.cfg.name, "version": c.cfg.version},
	}
	if c.cfg.token != "" {
		params["token"] = c.cfg.token
	}
	raw, err := c.request(ctx, "initialize", params)
	if err != nil {
		return fmt.Errorf("aikoql: initialize: %w", err)
	}
	var res struct {
		ServerInfo struct {
			Version string `json:"version"`
		} `json:"serverInfo"`
	}
	if err := json.Unmarshal(raw, &res); err != nil {
		return fmt.Errorf("aikoql: initialize response: %w", err)
	}
	if versionLess(parseVersion(res.ServerInfo.Version), parseVersion(MIN_SERVER_VERSION)) {
		return &McpError{
			Code:       "VERSION_MISMATCH",
			Message:    fmt.Sprintf("server version %q is older than the SDK minimum %s", res.ServerInfo.Version, MIN_SERVER_VERSION),
			Suggestion: "Upgrade the aikoql-mcp server to a supported version",
		}
	}
	return nil
}

// SessionParams establishes session identity (MRFC-0040). On TCP the
// identity is server-assigned by --tcp-token, so AgentID must be omitted
// there — only RunID is per-session.
type SessionParams struct {
	AgentID string   `json:"agent_id,omitempty"`
	RunID   string   `json:"run_id,omitempty"`
	Tenant  string   `json:"tenant,omitempty"`
	Roles   []string `json:"roles,omitempty"`
}

// SessionInit establishes session identity; subsequent calls inherit it.
func (c *Client) SessionInit(ctx context.Context, p SessionParams) error {
	_, err := c.request(ctx, "session/init", p)
	if err != nil {
		return fmt.Errorf("aikoql: session/init: %w", err)
	}
	return nil
}

// CallTool calls any registered MCP tool by name and returns its data
// payload — the escape hatch for tools without a typed wrapper here.
func (c *Client) CallTool(ctx context.Context, name string, arguments any) (json.RawMessage, error) {
	params := map[string]any{"name": name}
	if arguments != nil {
		params["arguments"] = arguments
	}
	raw, err := c.request(ctx, "tools/call", params)
	if err != nil {
		return nil, fmt.Errorf("aikoql: %s: %w", name, err)
	}
	var env struct {
		Content []struct {
			Text string `json:"text"`
		} `json:"content"`
		IsError bool `json:"isError"`
	}
	if err := json.Unmarshal(raw, &env); err != nil {
		return nil, fmt.Errorf("aikoql: %s envelope: %w", name, err)
	}
	text := ""
	if len(env.Content) > 0 {
		text = env.Content[0].Text
	}
	var data struct {
		OK    *bool           `json:"ok"`
		Data  json.RawMessage `json:"data"`
		Error *McpError       `json:"error"`
	}
	if err := json.Unmarshal([]byte(text), &data); err != nil {
		return nil, fmt.Errorf("aikoql: %s payload: %w", name, err)
	}
	// Mirrors the Python SDK: an absent "ok" means success; the "data"
	// field, when present, wraps the payload.
	if data.OK != nil && !*data.OK {
		if data.Error != nil {
			return nil, fmt.Errorf("aikoql: %s: %w", name, data.Error)
		}
		return nil, &McpError{Code: "INTERNAL", Message: fmt.Sprintf("tool %s failed without an error envelope", name)}
	}
	if len(data.Data) > 0 {
		return data.Data, nil
	}
	return json.RawMessage(text), nil
}

// parseVersion mirrors the Python SDK's dotted-int tuple: non-numeric
// segments become -1 (never >=).
func parseVersion(v string) []int {
	parts := []int{}
	for _, seg := range strings.Split(v, ".") {
		n, err := strconv.Atoi(seg)
		if err != nil {
			parts = append(parts, -1)
			continue
		}
		parts = append(parts, n)
	}
	return parts
}

// versionLess compares two dotted version tuples segment by segment.
func versionLess(a, b []int) bool {
	for i := 0; i < len(a) && i < len(b); i++ {
		if a[i] != b[i] {
			return a[i] < b[i]
		}
	}
	return len(a) < len(b)
}
