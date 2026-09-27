package aikoql

// Integration test against the REAL aikoql-mcp server — the proof that
// the Go SDK works as a client of the actual database. Skipped unless
// AIKOQL_MCP_BIN names the built binary (CI and laptop smoke both set it).

import (
	"bytes"
	"context"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

func startRealServer(t *testing.T, bin string) (addr string, stop func()) {
	t.Helper()
	// A free port: bind :0, remember the port, release it. The server
	// binds it again moments later — the dial retry below absorbs the
	// small race window.
	probe, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("probe listen: %v", err)
	}
	port := probe.Addr().(*net.TCPAddr).Port
	if err := probe.Close(); err != nil {
		t.Fatalf("probe close: %v", err)
	}
	addr = net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	// A non-existent path: the server auto-creates it as an aikoql-v2
	// database (SE2-M41). An existing-but-empty dir is REFUSED with
	// "not an aikoql-v2 database (no CURRENT)" — the live-server probe
	// caught this.
	dbDir := filepath.Join(t.TempDir(), "kb")
	var stderr bytes.Buffer
	cmd := exec.Command(bin, "serve",
		"--listen", addr,
		"--tcp-token", "s3cret:acme:admin",
		dbDir)
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("spawn %s: %v", bin, err)
	}
	stop = func() {
		if cmd.Process != nil {
			if err := cmd.Process.Kill(); err != nil {
				t.Logf("kill mcp server: %v", err)
			}
		}
		if err := cmd.Wait(); err != nil {
			t.Logf("wait mcp server: %v", err)
		}
	}
	// Readiness: retry Dial until the server accepts (stderr is surfaced
	// in the failure message if it never does).
	var lastErr error
	for i := 0; i < 100; i++ {
		conn, err := net.DialTimeout("tcp", addr, time.Second)
		if err == nil {
			if cerr := conn.Close(); cerr != nil {
				t.Fatalf("readiness conn close: %v", cerr)
			}
			return addr, stop
		}
		lastErr = err
		time.Sleep(100 * time.Millisecond)
	}
	stop()
	t.Fatalf("server never came up on %s: %v\nstderr:\n%s", addr, lastErr, stderr.String())
	return "", nil
}

func TestRealServerRoundTrip(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server integration test skipped")
	}
	addr, stop := startRealServer(t, bin)
	defer stop()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("s3cret"), WithClientInfo("go-integration", "0.0.0-test"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()

	// The version contract against the live server: initialize must pass
	// (the built binary is >= MIN_SERVER_VERSION) and the token must be
	// accepted (a wrong token fails the handshake).
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize against the real server: %v", err)
	}

	// remember → get → query → forget, end to end.
	got, err := c.Remember(ctx, RememberParams{
		TypeName:   "person",
		Properties: map[string]any{"name": "ada", "age": 37},
	})
	if err != nil {
		t.Fatalf("Remember: %v", err)
	}
	if got.KOID == "" {
		t.Fatal("Remember returned an empty KOID")
	}

	ko, err := c.Get(ctx, got.KOID, "")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	name, ok := ko.Properties["name"]
	if !ok || name != "ada" {
		t.Fatalf("Get properties wrong: %+v", ko.Properties)
	}

	raw, err := c.Aikoql(ctx, "MATCH person WHERE name == \"ada\" RETURN *", "")
	if err != nil {
		t.Fatalf("Aikoql: %v", err)
	}
	if len(raw) == 0 || !bytes.Contains(raw, []byte("person")) {
		t.Fatalf("Aikoql result unexpected: %s", raw)
	}

	// health + metrics prove the server-side DB is reachable end to end.
	if _, err := c.Health(ctx); err != nil {
		t.Fatalf("Health: %v", err)
	}
	metrics, err := c.Metrics(ctx)
	if err != nil {
		t.Fatalf("Metrics: %v", err)
	}
	if metrics.TotalObjects < 1 {
		t.Fatalf("Metrics should see the remembered object: %+v", metrics)
	}

	if _, err := c.Forget(ctx, got.KOID, "tombstone", ""); err != nil {
		t.Fatalf("Forget: %v", err)
	}
}

func TestRealServerRejectsWrongToken(t *testing.T) {
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — real-server integration test skipped")
	}
	addr, stop := startRealServer(t, bin)
	defer stop()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("wrong-token"))
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err == nil {
		t.Fatal("Initialize with a wrong token must fail")
	}
}
