package aikoql

// D-16 fault matrix (line wire): the §18 fault proxy sits between the SDK
// and a real server and mangles the newline-delimited JSON-RPC wire (§7 of
// the testing plan: every SDK passes the same matrix against the same
// misbehaving server). Frame accounting: client line #1 = initialize, #2 =
// the victim call, #3 = the follow-up (no HELLO/AUTH frames on the MCP
// wire). Mirrors crates/sdk/rust/tests/fault.rs on the line wire.
//
// The contracts GREEN must make hold (the proxy --wire line arm + the
// client's MAX_FRAME cap and closed latch):
//   drop-request/drop-response → victim TIMEOUT (retryable), follow-up ok
//   delay-response → tight deadline TIMEOUT; generous deadline ok
//   duplicate-response → both ok (id correlation skips the duplicate)
//   reorder-response → victim TIMEOUT (response held), follow-up ok
//   truncate-frame → victim TIMEOUT (the missing bytes never arrive),
//     follow-up ok (the wire is self-delimiting)
//   corrupt-frame → the line is noise per the frozen §3.3 semantics →
//     victim TIMEOUT, follow-up ok
//   inject-notification → both ok (an id-less frame is never a response)
//   inject-stale-response → both ok (stale id skipped)
//   close/half-close → victim ok, follow-up fails fast (never TIMEOUT),
//     the client latches closed → UNAVAILABLE from then on
//   slow-server → tight deadline TIMEOUT
//   oversized-response → FRAME_TOO_LARGE before buffering past the 1 MiB
//     cap (§19: a malicious server cannot cause unbounded client memory),
//     follow-up UNAVAILABLE (the stream is desynced — latched)

import (
	"bytes"
	"context"
	"errors"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

// proxyBin names the fault proxy: AIKOQL_FAULT_PROXY or the debug build.
func proxyBin(t *testing.T) string {
	t.Helper()
	if p := os.Getenv("AIKOQL_FAULT_PROXY"); p != "" {
		return p
	}
	base := filepath.Join("..", "..", "..", "target", "debug", "aikoql-fault-proxy")
	for _, p := range []string{base + ".exe", base} {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	t.Skip("aikoql-fault-proxy not built — run cargo build")
	return ""
}

// faultEnv brings up a real server + one proxy instance (one fault mode)
// and returns the proxy address. The port-probe idiom races → 3 attempts.
func faultEnv(t *testing.T, bin, proxy, mode string, extra ...string) (addr string, stop func()) {
	t.Helper()
	for attempt := 0; attempt < 3; attempt++ {
		probe, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatalf("probe listen: %v", err)
		}
		srvPort := probe.Addr().(*net.TCPAddr).Port
		if err := probe.Close(); err != nil {
			t.Fatalf("probe close: %v", err)
		}
		srvAddr := net.JoinHostPort("127.0.0.1", strconv.Itoa(srvPort))
		var srvErr bytes.Buffer
		srv := exec.Command(bin, "serve",
			"--listen", srvAddr,
			"--tcp-token", "s3cret:acme:admin",
			filepath.Join(t.TempDir(), "kb"))
		srv.Stderr = &srvErr
		if err := srv.Start(); err != nil {
			t.Fatalf("spawn server: %v", err)
		}
		if up, err := waitUp(srvAddr, nil, &srvErr); !up {
			kill(srv)
			t.Fatalf("server never came up: %v", err)
		}

		probe2, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatalf("probe listen: %v", err)
		}
		pxPort := probe2.Addr().(*net.TCPAddr).Port
		if err := probe2.Close(); err != nil {
			t.Fatalf("probe close: %v", err)
		}
		pxAddr := net.JoinHostPort("127.0.0.1", strconv.Itoa(pxPort))
		args := []string{"--listen", pxAddr, "--target", srvAddr, "--wire", "line", "--mode", mode}
		args = append(args, extra...)
		var pxErr bytes.Buffer
		px := exec.Command(proxy, args...)
		px.Stderr = &pxErr
		if err := px.Start(); err != nil {
			kill(srv)
			t.Fatalf("spawn proxy: %v", err)
		}
		// The proxy can die before it ever binds (e.g. the unknown-flag
		// panic) — waitUp must stop dialing and surface its stderr.
		pxDone := make(chan struct{})
		go func() {
			_, _ = px.Process.Wait()
			close(pxDone)
		}()
		if up, err := waitUp(pxAddr, pxDone, &pxErr); up {
			stop = func() {
				kill(px)
				kill(srv)
			}
			return pxAddr, stop
		} else {
			kill(px)
			kill(srv)
			if attempt == 2 {
				t.Fatalf("proxy never came up: %v", err)
			}
		}
	}
	return "", nil
}

// waitUp polls a dial retry until addr accepts; a reaped process (or its
// stderr on the never-came-up path) rides the error. A nil reaped channel
// disables the death check — the server side never dies.
func waitUp(addr string, reaped <-chan struct{}, stderr *bytes.Buffer) (bool, error) {
	var lastErr error
	for i := 0; i < 100; i++ {
		conn, err := net.DialTimeout("tcp", addr, time.Second)
		if err == nil {
			if cerr := conn.Close(); cerr != nil {
				return false, cerr
			}
			return true, nil
		}
		lastErr = err
		select {
		case <-reaped:
			return false, errors.New("process exited:\n" + stderr.String())
		default:
		}
		time.Sleep(100 * time.Millisecond)
	}
	return false, errors.New("no listener on " + addr + ": " + lastErr.Error() + "\nstderr:\n" + stderr.String())
}

func kill(cmd *exec.Cmd) {
	if cmd.Process != nil {
		if err := cmd.Process.Kill(); err != nil {
			// A process that already exited reports an error on Windows —
			// Wait below reaps either way.
			_ = err
		}
	}
	_ = cmd.Wait()
}

// dialClient connects through the proxy: initialize is client line #1.
func dialClient(t *testing.T, addr string) *Client {
	t.Helper()
	c, err := Dial(context.Background(), addr, WithToken("s3cret"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	if err := c.Initialize(context.Background()); err != nil {
		t.Fatalf("Initialize: %v", err)
	}
	return c
}

// victimCall runs the health call under a tight deadline (the fault hits
// client line #2). Returns the error's code, or "" when it succeeds.
func victimCall(t *testing.T, c *Client, ms int) string {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(ms)*time.Millisecond)
	defer cancel()
	_, err := c.CallTool(ctx, "health", nil)
	var me *McpError
	if errors.As(err, &me) {
		return me.Code
	}
	if err != nil {
		return "raw:" + err.Error()
	}
	return ""
}

func wantTimeout(t *testing.T, got string) {
	t.Helper()
	if got != "TIMEOUT" {
		t.Fatalf("victim: want TIMEOUT, got %q", got)
	}
}

// followUp runs the health call with no deadline (client line #3).
func followUp(t *testing.T, c *Client) string {
	t.Helper()
	_, err := c.CallTool(context.Background(), "health", nil)
	var me *McpError
	if errors.As(err, &me) {
		return me.Code
	}
	if err != nil {
		return "raw:" + err.Error()
	}
	return ""
}

func TestFaultMatrix(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server fault matrix skipped")
	}
	proxy := proxyBin(t)

	t.Run("drop-request", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "drop-request", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if got := followUp(t, c); got != "" {
			t.Fatalf("follow-up should survive: got %q", got)
		}
	})

	t.Run("drop-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "drop-response", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if got := followUp(t, c); got != "" {
			t.Fatalf("follow-up should survive: got %q", got)
		}
	})

	t.Run("delay-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "delay-response", "--from", "2", "--delay-ms", "400")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if err := c.Close(); err != nil {
			t.Fatalf("Close: %v", err)
		}
		addr2, stop2 := faultEnv(t, bin, proxy, "delay-response", "--from", "2", "--delay-ms", "400")
		defer stop2()
		c2 := dialClient(t, addr2)
		if got := victimCall(t, c2, 2000); got != "" {
			t.Fatalf("a generous deadline absorbs the delay: got %q", got)
		}
	})

	t.Run("duplicate-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "duplicate-response", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 2000); got != "" {
			t.Fatalf("victim: got %q", got)
		}
		if got := followUp(t, c); got != "" {
			t.Fatalf("the duplicate is a stale id — skipped: got %q", got)
		}
	})

	t.Run("reorder-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "reorder-response", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if got := followUp(t, c); got != "" {
			t.Fatalf("request #3 releases response #2: got %q", got)
		}
	})

	t.Run("truncate-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "truncate-response", "--n", "2", "--bytes", "8")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if got := followUp(t, c); got != "" {
			t.Fatalf("the wire is self-delimiting: got %q", got)
		}
	})

	t.Run("corrupt-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "corrupt-response", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
		if got := followUp(t, c); got != "" {
			t.Fatalf("noise-skip, never a fast error: got %q", got)
		}
	})

	t.Run("inject-notification", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "inject-notification", "--after", "2")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 2000); got != "" {
			t.Fatalf("victim: got %q", got)
		}
		if got := followUp(t, c); got != "" {
			t.Fatalf("an id-less frame is never a response: got %q", got)
		}
	})

	t.Run("inject-stale-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "inject-stale-response", "--after", "2")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 2000); got != "" {
			t.Fatalf("victim: got %q", got)
		}
		if got := followUp(t, c); got != "" {
			t.Fatalf("the replayed initialize response is stale: got %q", got)
		}
	})

	t.Run("close-after", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "close-after", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 2000); got != "" {
			t.Fatalf("victim: got %q", got)
		}
		first := followUp(t, c)
		if first == "" {
			t.Fatal("follow-up should fail — the connection is closed")
		}
		if first == "TIMEOUT" {
			t.Fatal("follow-up must fail fast, never TIMEOUT")
		}
		if third := followUp(t, c); third != "UNAVAILABLE" {
			t.Fatalf("latched: a dead conn never hangs — want UNAVAILABLE, got %q", third)
		}
	})

	t.Run("half-close-after", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "half-close-after", "--n", "2")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 2000); got != "" {
			t.Fatalf("victim: got %q", got)
		}
		first := followUp(t, c)
		if first == "" {
			t.Fatal("follow-up should fail — the connection is half-closed")
		}
		if first == "TIMEOUT" {
			t.Fatal("follow-up must fail fast, never TIMEOUT")
		}
		if third := followUp(t, c); third != "UNAVAILABLE" {
			t.Fatalf("latched: a dead conn never hangs — want UNAVAILABLE, got %q", third)
		}
	})

	t.Run("slow-server", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "slow-server", "--from", "2", "--bytes", "4", "--delay-ms", "25")
		defer stop()
		c := dialClient(t, addr)
		wantTimeout(t, victimCall(t, c, 200))
	})

	t.Run("oversized-response", func(t *testing.T) {
		addr, stop := faultEnv(t, bin, proxy, "oversized-response", "--n", "2", "--claim", "67108864")
		defer stop()
		c := dialClient(t, addr)
		if got := victimCall(t, c, 5000); got != "FRAME_TOO_LARGE" {
			t.Fatalf("the 1 MiB cap trips before buffering 64 MiB: got %q", got)
		}
		if third := followUp(t, c); third != "UNAVAILABLE" {
			t.Fatalf("the stream is desynced — latched: want UNAVAILABLE, got %q", third)
		}
	})
}
