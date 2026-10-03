package aikoql

// D-07: the review's §3.3 case list against the Go transport. Case 1
// (notification before the response) is pinned by TestRequestSkipsNotifications
// in client_test.go; cases 2-7 live here. The contract, aligned with the
// Python SDK's D-06 semantics:
//
//  2. notification + response in one TCP chunk — split, never misread
//  3. response for another (impossible, higher) id — PROTOCOL_ERROR
//  4. duplicate response — skipped, the next request is unaffected
//  5. missing response — the frozen TIMEOUT (retryable) at the deadline
//  6. late response after timeout — skipped, cannot corrupt the next request
//  7. malformed frame before a valid response — skipped, response matched
//
// Stale frames (id < expected) are skips, never errors: a duplicate and a
// late response are indistinguishable and equally harmless to a serialized
// client.

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"net"
	"testing"
	"time"
)

// rawServer serves one connection and writes each script entry verbatim
// after reading one request line — raw bytes so malformed frames and
// multi-frame chunks can be scripted (the Python SDK's ScriptedServer).
func rawServer(t *testing.T, script ...[]byte) string {
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
		for _, frame := range script {
			if _, err := r.ReadString('\n'); err != nil {
				return // client closed
			}
			if _, err := conn.Write(frame); err != nil {
				return
			}
		}
	}()
	return ln.Addr().String()
}

func mustDial(t *testing.T, addr string) *Client {
	t.Helper()
	c, err := Dial(context.Background(), addr)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	t.Cleanup(func() { c.Close() })
	return c
}

func frameLine(f rpcFrame) []byte {
	data, _ := json.Marshal(f)
	return append(data, '\n')
}

func TestCorrelationNotificationAndResponseInOneChunk(t *testing.T) {
	notify := frameLine(rpcFrame{JSONRPC: "2.0", Method: "notifications/notify",
		Params: json.RawMessage(`{"event":"audit"}`)})
	resp := frameLine(rpcFrame{JSONRPC: "2.0", ID: 1,
		Result: infoReply("")})
	addr := rawServer(t, append(notify, resp...)) // one write, two frames
	c := mustDial(t, addr)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize through a same-chunk notification: %v", err)
	}
}

func TestCorrelationResponseForAnotherRequestIsProtocolError(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		return []rpcFrame{{JSONRPC: "2.0", ID: req.ID + 5, Result: json.RawMessage(`{}`)}}
	})
	c := mustDial(t, addr)
	// A deadline bounds the RED: the current code skips the foreign id and
	// would otherwise block forever. GREEN raises PROTOCOL_ERROR at once.
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, err := c.request(ctx, "get", nil)
	var me *McpError
	if !errors.As(err, &me) || me.Code != "PROTOCOL_ERROR" {
		t.Fatalf("expected PROTOCOL_ERROR McpError, got %v", err)
	}
}

func TestCorrelationDuplicateResponseDoesNotPoisonNextRequest(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		if req.ID == 1 {
			return []rpcFrame{{JSONRPC: "2.0", ID: 1, Result: json.RawMessage(`{"first":true}`)}}
		}
		return []rpcFrame{
			{JSONRPC: "2.0", ID: 1, Result: json.RawMessage(`{"stale":true}`)}, // duplicate of call 1
			{JSONRPC: "2.0", ID: 2, Result: json.RawMessage(`{"fresh":true}`)},
		}
	})
	c := mustDial(t, addr)
	first, err := c.request(context.Background(), "get", nil)
	if err != nil || string(first) != `{"first":true}` {
		t.Fatalf("call 1: %v %s", err, first)
	}
	second, err := c.request(context.Background(), "get", nil)
	if err != nil || string(second) != `{"fresh":true}` {
		t.Fatalf("call 2 must skip the duplicate: %v %s", err, second)
	}
}

func TestCorrelationMissingResponseTimesOut(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame { return nil })
	c := mustDial(t, addr)
	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()
	_, err := c.request(ctx, "get", nil)
	var me *McpError
	if !errors.As(err, &me) || me.Code != "TIMEOUT" || !me.Retryable {
		t.Fatalf("expected retryable TIMEOUT McpError, got %v", err)
	}
}

func TestCorrelationLateResponseAfterTimeoutIsSkipped(t *testing.T) {
	addr := fakeServer(t, func(t *testing.T, req rpcFrame) []rpcFrame {
		if req.ID == 1 {
			return nil // the client's deadline fires first
		}
		return []rpcFrame{
			{JSONRPC: "2.0", ID: 1, Result: json.RawMessage(`{"late":true}`)}, // stale: late response for call 1
			{JSONRPC: "2.0", ID: 2, Result: json.RawMessage(`{"fresh":true}`)},
		}
	})
	c := mustDial(t, addr)
	ctx1, cancel1 := context.WithTimeout(context.Background(), 300*time.Millisecond)
	_, err := c.request(ctx1, "get", nil)
	cancel1()
	var me *McpError
	if !errors.As(err, &me) || me.Code != "TIMEOUT" {
		t.Fatalf("call 1 must time out, got %v", err)
	}
	second, err := c.request(context.Background(), "get", nil)
	if err != nil || string(second) != `{"fresh":true}` {
		t.Fatalf("call 2 must skip the late frame: %v %s", err, second)
	}
}

func TestCorrelationMalformedFrameBeforeValidResponse(t *testing.T) {
	resp := frameLine(rpcFrame{JSONRPC: "2.0", ID: 1, Result: json.RawMessage(`{"ok":1}`)})
	script := append([]byte("this is not json\n"), resp...)
	addr := rawServer(t, script)
	c := mustDial(t, addr)
	got, err := c.request(context.Background(), "get", nil)
	if err != nil || string(got) != `{"ok":1}` {
		t.Fatalf("the valid response behind a malformed frame: %v %s", err, got)
	}
}
