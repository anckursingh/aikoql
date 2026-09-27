package aikoql

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"net"
	"strings"
	"testing"
	"time"
)

// rpcFrame is one newline-delimited JSON-RPC frame the fake server reads
// or writes. Fields are loose so canned frames can be partial.
type rpcFrame struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int             `json:"id,omitempty"`
	Method  string          `json:"method,omitempty"`
	Params  json.RawMessage `json:"params,omitempty"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   json.RawMessage `json:"error,omitempty"`
}

// fakeServer listens on 127.0.0.1:0 and serves one connection: it reads
// request frames until the client closes, answering each through respond,
// then closes. Returns the dial address.
func fakeServer(t *testing.T, respond func(t *testing.T, req rpcFrame) []rpcFrame) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		r := bufio.NewReader(conn)
		for {
			line, err := r.ReadString('\n')
			if err != nil {
				return // client closed
			}
			var req rpcFrame
			if err := json.Unmarshal([]byte(line), &req); err != nil {
				return
			}
			for _, frame := range respond(t, req) {
				data, err := json.Marshal(frame)
				if err != nil {
					return
				}
				if _, err := conn.Write(append(data, '\n')); err != nil {
					return
				}
			}
		}
	}()
	return ln.Addr().String()
}

func toolResult(payload any) json.RawMessage {
	text, _ := json.Marshal(payload)
	env, _ := json.Marshal(map[string]any{"content": []map[string]any{{"text": string(text)}}})
	return env
}

func TestDialContextCanceled(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := Dial(ctx, "127.0.0.1:1"); err == nil {
		t.Fatal("Dial with a canceled context must fail")
	}
}

func TestInitializeSendsTokenAndClientInfo(t *testing.T) {
	var sawToken string
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		if req.Method != "initialize" {
			t.Fatalf("expected initialize, got %q", req.Method)
		}
		var p struct {
			Token      string `json:"token"`
			ClientInfo struct {
				Name    string `json:"name"`
				Version string `json:"version"`
			} `json:"clientInfo"`
		}
		if err := json.Unmarshal(req.Params, &p); err != nil {
			t.Fatalf("initialize params: %v", err)
		}
		sawToken = p.Token
		return []rpcFrame{{
			JSONRPC: "2.0", ID: req.ID,
			Result: json.RawMessage(`{"serverInfo":{"name":"aikoql-mcp","version":"0.1.19"}}`),
		}}
	})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-test", "1.2.3"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize: %v", err)
	}
	if sawToken != "s3cret" {
		t.Fatalf("token not sent: %q", sawToken)
	}
}

func TestInitializeVersionMismatch(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		return []rpcFrame{{
			JSONRPC: "2.0", ID: req.ID,
			Result: json.RawMessage(`{"serverInfo":{"name":"aikoql-mcp","version":"0.1.18"}}`),
		}}
	})
	ctx := context.Background()
	c, err := Dial(ctx, addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	err = c.Initialize(ctx)
	var me *McpError
	if !errors.As(err, &me) || me.Code != "VERSION_MISMATCH" {
		t.Fatalf("expected VERSION_MISMATCH McpError, got %v", err)
	}
}

func TestToolErrorEnvelope(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		return []rpcFrame{{
			JSONRPC: "2.0", ID: req.ID,
			Result: toolResult(map[string]any{
				"ok": false,
				"error": map[string]any{
					"code": "NOT_FOUND", "message": "no such ko",
					"retryable": true, "suggestion": "check the koid",
				},
			}),
		}}
	})
	ctx := context.Background()
	c, err := Dial(ctx, addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	_, err = c.Get(ctx, "ko", "")
	var me *McpError
	if !errors.As(err, &me) {
		t.Fatalf("expected McpError, got %v", err)
	}
	if me.Code != "NOT_FOUND" || !me.Retryable || me.Suggestion != "check the koid" {
		t.Fatalf("McpError fields wrong: %+v", me)
	}
	if !strings.Contains(err.Error(), "no such ko") {
		t.Fatalf("error text must carry the server message: %v", err)
	}
}

func TestRememberGetRoundTrip(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		var call struct {
			Name      string `json:"name"`
			Arguments struct {
				TypeName   string `json:"type_name"`
				Properties struct {
					Name string `json:"name"`
				} `json:"properties"`
			} `json:"arguments"`
		}
		if err := json.Unmarshal(req.Params, &call); err != nil {
			t.Fatalf("tools/call params: %v", err)
		}
		switch call.Name {
		case "remember":
			if call.Arguments.TypeName != "person" || call.Arguments.Properties.Name != "ada" {
				t.Fatalf("remember args wrong: %+v", call.Arguments)
			}
			return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
				Result: toolResult(map[string]any{
					"ok": true, "data": map[string]any{"koid": "abc123", "version": 2, "commit_ts": 99},
				})}}
		case "get":
			return []rpcFrame{{JSONRPC: "2.0", ID: req.ID,
				Result: toolResult(map[string]any{
					"ok": true, "data": map[string]any{
						"koid": "abc123", "version": 2,
						"properties": map[string]any{"name": "ada"},
					},
				})}}
		default:
			t.Fatalf("unexpected tool %q", call.Name)
			return nil
		}
	})
	ctx := context.Background()
	c, err := Dial(ctx, addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	got, err := c.Remember(ctx, RememberParams{TypeName: "person", Properties: map[string]any{"name": "ada"}})
	if err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if got.KOID != "abc123" || got.Version != 2 {
		t.Fatalf("remembered wrong: %+v", got)
	}
	ko, err := c.Get(ctx, "abc123", "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if ko.KOID != "abc123" || ko.Properties["name"] != "ada" {
		t.Fatalf("get wrong: %+v", ko)
	}
}

func TestRequestSkipsNotifications(t *testing.T) {
	// The server may push notify frames between request and response (e.g.
	// audit events). The client must correlate by id, not by order.
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		return []rpcFrame{
			{JSONRPC: "2.0", Method: "notifications/notify",
				Params: json.RawMessage(`{"event":"audit"}`)},
			{JSONRPC: "2.0", ID: req.ID, Result: json.RawMessage(`{"serverInfo":{"version":"0.1.19"}}`)},
		}
	})
	ctx := context.Background()
	c, err := Dial(ctx, addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize through a pushed notification: %v", err)
	}
}

func TestAikoqlStreamChunks(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		if req.Method != "aikoql/stream" {
			t.Fatalf("expected aikoql/stream, got %q", req.Method)
		}
		return []rpcFrame{
			{JSONRPC: "2.0", ID: req.ID,
				Result: json.RawMessage(`{"stream_id":"s1","total_chunks":2}`)},
			{JSONRPC: "2.0", Method: "notifications/notify",
				Params: json.RawMessage(`{"stream_id":"s1","results":[{"koid":"a"}],"done":false}`)},
			{JSONRPC: "2.0", Method: "notifications/notify",
				Params: json.RawMessage(`{"stream_id":"s1","results":[{"koid":"b"}],"done":true}`)},
		}
	})
	ctx := context.Background()
	c, err := Dial(ctx, addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	var chunks []string
	if err := c.AikoqlStream(ctx, "MATCH person RETURN *", "", func(raw json.RawMessage) error {
		chunks = append(chunks, string(raw))
		return nil
	}); err != nil {
		t.Fatalf("AikoqlStream: %v", err)
	}
	if len(chunks) != 2 {
		t.Fatalf("expected 2 chunks, got %d: %v", len(chunks), chunks)
	}
	if !strings.Contains(chunks[1], `"done":true`) {
		t.Fatalf("last chunk must be the done frame: %v", chunks[1])
	}
}
