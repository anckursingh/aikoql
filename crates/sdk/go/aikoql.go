// Package aikoql is the Go SDK for the AikoQL knowledge database. It
// speaks MCP JSON-RPC over TCP or stdio to an aikoql-mcp server — the
// same first-class surface the Python SDK and standard MCP clients use:
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
// DialStdio spawns the server instead and speaks MCP over its
// stdin/stdout — the docker-container contract (`docker run -i --rm
// image serve /data/aikoql.redb`), token-free because stdio trusts the
// process boundary.
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
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"
)

// MIN_SERVER_VERSION is the oldest aikoql-mcp server this SDK will talk
// to (the ND-12 version contract, mirrored from the Python SDK; pinned
// against the workspace version by the go-sdk CI job).
const MIN_SERVER_VERSION = "0.2.2"

// Version is the SDK's own version, advertised as the client identity by
// default. Distributors pin it at build time: -ldflags
// "-X github.com/anckursingh/aikoql/sdk/go.Version=<version>". The default
// is the honest marker — a library cannot know its own module version at
// runtime (debug.ReadBuildInfo reports the MAIN module's).
var Version = "dev"

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

// transport abstracts the frame stream: a TCP conn or a spawned server's
// stdio pipes. Two implementations — the SDK's one justified interface.
type transport interface {
	Write(p []byte) (int, error)
	Close() error
	SetDeadline(t time.Time) error
}

// Client is one MCP JSON-RPC connection to an aikoql-mcp server.
type Client struct {
	tr     transport
	r      *bufio.Reader
	mu     sync.Mutex // serializes frames: one in-flight call per conn
	nextID int64
	closed bool
	cfg    clientConfig
}

// Dial opens a TCP connection to addr ("host:port"). The handshake is
// separate (Initialize) so a pooled Client can dial first and authenticate
// later; every aikoql-mcp TCP server requires the token.
func Dial(ctx context.Context, addr string, opts ...Option) (*Client, error) {
	cfg := clientConfig{name: "aikoql-go-sdk", version: Version, dialTimeout: 5 * time.Second}
	for _, o := range opts {
		o(&cfg)
	}
	d := net.Dialer{Timeout: cfg.dialTimeout}
	conn, err := d.DialContext(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("aikoql: dial %s: %w", addr, err)
	}
	return &Client{tr: conn, r: bufio.NewReader(conn), cfg: cfg}, nil
}

// DialStdio spawns bin with args and speaks MCP over its stdin/stdout.
// This is the docker-container contract — `docker run -i --rm image serve
// /data/aikoql.redb` runs the same binary over the same stdio (the repo's
// e2e-volume-restart.js proves the container side). No token: stdio trusts
// the process boundary and the server's stdio mode needs no --tcp-token.
// The server's stderr is inherited so its logs stay visible. ctx bounds
// the spawn only — the process lives until Close, and per-call ctxs carry
// the call deadlines (a hung stdio server is killed at the deadline: a
// pipe has no socket deadline to set).
func DialStdio(ctx context.Context, bin string, args ...string) (*Client, error) {
	cfg := clientConfig{name: "aikoql-go-sdk", version: Version, dialTimeout: 5 * time.Second}
	cmd := exec.Command(bin, args...)
	cmd.Stderr = os.Stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("aikoql: stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("aikoql: stdout pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("aikoql: start %s: %w", bin, err)
	}
	if err := ctx.Err(); err != nil {
		killErr := cmd.Process.Kill()
		waitErr := cmd.Wait()
		return nil, errors.Join(err, killErr, waitErr)
	}
	return &Client{tr: &stdioTransport{cmd: cmd, stdin: stdin}, r: bufio.NewReader(stdout), cfg: cfg}, nil
}

// stdioTransport adapts a spawned server's stdin/stdout to the transport
// contract. Deadlines become a kill timer: a hung stdio server cannot be
// given a socket deadline, and a killed child unblocks the read with an
// error.
type stdioTransport struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	cancel func() // stops the armed kill timer
}

func (s *stdioTransport) Write(p []byte) (int, error) {
	return s.stdin.Write(p)
}

func (s *stdioTransport) SetDeadline(t time.Time) error {
	if s.cancel != nil {
		s.cancel()
		s.cancel = nil
	}
	if t.IsZero() {
		return nil
	}
	d := time.Until(t)
	if d <= 0 {
		if err := s.cmd.Process.Kill(); err != nil {
			return fmt.Errorf("aikoql: kill %s: %w", s.cmd.Path, err)
		}
		return context.DeadlineExceeded
	}
	timer := time.AfterFunc(d, func() {
		if err := s.cmd.Process.Kill(); err != nil {
			fmt.Fprintf(os.Stderr, "aikoql: deadline kill %s: %v\n", s.cmd.Path, err)
		}
	})
	s.cancel = func() { timer.Stop() }
	return nil
}

// Close shuts the stdio server down gracefully: stdin EOF is the server's
// checkpoint-and-exit signal (main.rs stdio mode). A server that lingers
// past the grace window is killed.
func (s *stdioTransport) Close() error {
	cerr := s.stdin.Close()
	done := make(chan error, 1)
	go func() { done <- s.cmd.Wait() }()
	select {
	case waitErr := <-done:
		return errors.Join(cerr, waitErr)
	case <-time.After(2 * time.Second):
		killErr := s.cmd.Process.Kill()
		waitErr := <-done
		return errors.Join(cerr, killErr, waitErr)
	}
}

// Close closes the transport. Safe to call more than once.
func (c *Client) Close() error {
	c.mu.Lock()
	c.closed = true
	c.mu.Unlock()
	return c.tr.Close()
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
	return &McpError{Code: normalizeCode(string(e.Code)), Message: e.Message}
}

// normalizeCode maps a raw JSON-RPC code to the string code the SDK
// surfaces. A string-encoded code is decoded exactly as Python's
// json.loads does (escapes included); an absent code is INTERNAL.
// (FuzzErrorMapping.)
func normalizeCode(code string) string {
	if len(code) > 0 && code[0] == '"' {
		var s string
		if err := json.Unmarshal([]byte(code), &s); err == nil {
			code = s
		}
	}
	if code == "" {
		code = "INTERNAL"
	}
	return code
}

// maxFrame is the §19 cap: an unterminated line past it is refused before
// the buffer can grow past the bound (ReadString would allocate the whole
// line first, transiently violating the memory bound).
const maxFrame = 1 << 20

// readLine reads one newline-terminated line under the §19 cap. ReadSlice
// reuses the bufio buffer; ErrBufferFull means the line does not fit, so
// bounded pieces accumulate until the cap trips.
func (c *Client) readLine() (string, error) {
	var buf []byte
	for {
		part, err := c.r.ReadSlice('\n')
		buf = append(buf, part...)
		if len(buf) > maxFrame {
			return "", &McpError{
				Code:       "FRAME_TOO_LARGE",
				Message:    "response frame exceeds the 1 MiB cap",
				Suggestion: "The server sent an over-cap frame; reconnect.",
			}
		}
		if err != bufio.ErrBufferFull {
			return string(buf), err
		}
	}
}

// applyDeadline projects the context deadline onto the transport so a
// stalled server cannot hang the caller past ctx. The returned clear func
// disarms it when the call ends — on stdio the armed deadline is a kill
// timer and must not outlive the call it bounds.
func (c *Client) applyDeadline(ctx context.Context) (clear func() error, err error) {
	if err := ctx.Err(); err != nil {
		return nil, err // already cancelled — nothing is sent, the connection stays clean
	}
	if c.closed {
		// A call on a closed client fails observably (§7 principle 11: a
		// dead connection never deadlocks the caller) and a fresh Dial
		// recovers it. Checked before SetDeadline — the transport is gone
		// and would answer with a bare "use of closed network connection".
		return nil, &McpError{Code: "UNAVAILABLE",
			Message:    "the client is closed",
			Suggestion: "Connect again."}
	}
	var d time.Time
	if dl, ok := ctx.Deadline(); ok {
		d = dl
	}
	if err := c.tr.SetDeadline(d); err != nil {
		return nil, fmt.Errorf("aikoql: set deadline: %w", err)
	}
	return func() error { return c.tr.SetDeadline(time.Time{}) }, nil
}

// The frozen §3.3 response-id correlation rules: a smaller id is skipped
// (id-less, notification, duplicate, or late — never an error), a larger id
// is a PROTOCOL_ERROR, equality matches.
const (
	corrSkip = iota
	corrMatch
	corrProtocol
)

// classifyID applies the frozen correlation rules to one response id.
// (FuzzRequestIDCorrelation.)
func classifyID(want, got int64) int {
	switch {
	case got < want:
		return corrSkip
	case got > want:
		return corrProtocol
	default:
		return corrMatch
	}
}

// request sends one JSON-RPC request and returns its result frame,
// skipping pushed notifications by id correlation.
func (c *Client) request(ctx context.Context, method string, params any) (result json.RawMessage, err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	clear, err := c.applyDeadline(ctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := clear(); cerr != nil {
			err = errors.Join(err, cerr)
		}
	}()
	c.nextID++
	id := c.nextID
	frame, err := json.Marshal(rpcRequest{JSONRPC: "2.0", ID: id, Method: method, Params: params})
	if err != nil {
		return nil, fmt.Errorf("aikoql: marshal %s: %w", method, err)
	}
	if _, err := c.tr.Write(append(frame, '\n')); err != nil {
		c.closed = true // the transport is gone — latch
		return nil, fmt.Errorf("aikoql: send %s: %w", method, err)
	}
	for {
		line, err := c.readLine()
		if err != nil {
			// The transport deadline can fire a hair before the ctx timer
			// marks DeadlineExceeded — either one is the frozen TIMEOUT.
			if errors.Is(ctx.Err(), context.DeadlineExceeded) ||
				errors.Is(err, os.ErrDeadlineExceeded) {
				return nil, &McpError{
					Code:      "TIMEOUT",
					Message:   fmt.Sprintf("no response for request %d within the deadline", id),
					Retryable: true,
					Suggestion: "Retry with backoff; the request may have " +
						"committed.",
				}
			}
			// The transport failed (EOF, close, over-cap frame): latch —
			// later calls fail fast with UNAVAILABLE instead of writing
			// into a dead socket. TIMEOUT above does NOT latch.
			c.closed = true
			var me *McpError
			if errors.As(err, &me) {
				return nil, err // keep the code (FRAME_TOO_LARGE)
			}
			return nil, fmt.Errorf("aikoql: read %s: %w", method, err)
		}
		var resp rpcResponse
		if err := json.Unmarshal([]byte(line), &resp); err != nil {
			continue // tolerate non-JSON noise frames
		}
		switch classifyID(id, resp.ID) {
		case corrSkip:
			continue
		case corrProtocol:
			return nil, &McpError{
				Code:       "PROTOCOL_ERROR",
				Message:    fmt.Sprintf("response id %d does not match request %d", resp.ID, id),
				Suggestion: "Check SDK/server version pairing.",
			}
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
func (c *Client) stream(ctx context.Context, method string, params any, yieldFn func(chunk json.RawMessage) error) (err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	clear, err := c.applyDeadline(ctx)
	if err != nil {
		return err
	}
	defer func() {
		if cerr := clear(); cerr != nil {
			err = errors.Join(err, cerr)
		}
	}()
	c.nextID++
	id := c.nextID
	frame, err := json.Marshal(rpcRequest{JSONRPC: "2.0", ID: id, Method: method, Params: params})
	if err != nil {
		return fmt.Errorf("aikoql: marshal %s: %w", method, err)
	}
	if _, err := c.tr.Write(append(frame, '\n')); err != nil {
		c.closed = true // the transport is gone — latch
		return fmt.Errorf("aikoql: send %s: %w", method, err)
	}
	var streamID string
	total, received := 0, 0
	for {
		line, err := c.readLine()
		if err != nil {
			if errors.Is(ctx.Err(), context.DeadlineExceeded) ||
				errors.Is(err, os.ErrDeadlineExceeded) {
				return &McpError{
					Code:      "TIMEOUT",
					Message:   fmt.Sprintf("no response for request %d within the deadline", id),
					Retryable: true,
					Suggestion: "Retry with backoff; the request may have " +
						"committed.",
				}
			}
			c.closed = true
			var me *McpError
			if errors.As(err, &me) {
				return err
			}
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
				StreamID    string `json:"stream_id"`
				TotalChunks int    `json:"total_chunks"`
			}
			if err := json.Unmarshal(resp.Result, &head); err != nil {
				return fmt.Errorf("aikoql: %s stream head: %w", method, err)
			}
			streamID = head.StreamID
			total = head.TotalChunks
			// The response frame IS the first chunk (it carries the data;
			// for total_chunks == 1 there is no notify at all) — the Python
			// SDK yields it too, and the conformance transcript must match.
			if err := yieldFn(resp.Result); err != nil {
				return fmt.Errorf("aikoql: %s consumer: %w", method, err)
			}
			received++
			if total > 0 && received >= total {
				return nil // the whole stream arrived in the response frame
			}
			continue
		}
		if streamID == "" || resp.Method != "notifications/notify" {
			continue // push before the response, or an unrelated event
		}
		pStreamID, done, err := decodeStreamNotify(resp.Params)
		if err != nil {
			continue
		}
		if pStreamID != streamID {
			continue
		}
		if err := yieldFn(resp.Params); err != nil {
			return fmt.Errorf("aikoql: %s consumer: %w", method, err)
		}
		received++
		if done || (total > 0 && received >= total) {
			return nil // Python's exit condition: received == total_chunks, or done
		}
	}
}

// decodeStreamNotify decodes one notifications/notify params frame:
// stream_id plus the done flag. (FuzzDecodeStreamChunk.)
func decodeStreamNotify(params json.RawMessage) (streamID string, done bool, err error) {
	var p struct {
		StreamID string `json:"stream_id"`
		Done     bool   `json:"done"`
	}
	if err := json.Unmarshal(params, &p); err != nil {
		return "", false, err
	}
	return p.StreamID, p.Done, nil
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
	return decodeToolEnvelope(name, raw)
}

// decodeToolEnvelope decodes the MCP tools/call envelope: the first content
// text block carries the tool's own {ok, data, error} payload (the name is
// only used in error text). (FuzzDecodeToolEnvelope.)
func decodeToolEnvelope(name string, raw json.RawMessage) (json.RawMessage, error) {
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
