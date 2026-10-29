package aikoql

// The SDK over TLS against the real server started with --tls-cert /
// --tls-key (the MCP crate's test certificates, SAN IP:127.0.0.1):
// WithTLS(a config trusting the test CA) dials, initializes and
// round-trips a write; a config trusting another CA is refused at Dial,
// before the token is sent. Skips unless AIKOQL_MCP_BIN is set.

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"os"
	"path/filepath"
	"testing"
	"time"
)

var tlsFixtures = filepath.Join("..", "..", "services", "api", "mcp", "tests", "fixtures", "tls")

func trusting(t *testing.T, ca string) *tls.Config {
	t.Helper()
	pem, err := os.ReadFile(filepath.Join(tlsFixtures, ca))
	if err != nil {
		t.Fatalf("read %s: %v", ca, err)
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(pem) {
		t.Fatalf("no certificate in %s", ca)
	}
	return &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12}
}

func startTLSServer(t *testing.T) string {
	t.Helper()
	bin := os.Getenv("AIKOQL_MCP_BIN")
	if bin == "" {
		t.Skip("AIKOQL_MCP_BIN not set — TLS real-server test skipped")
	}
	addr, stop := startRealServer(t, bin,
		"--tls-cert", filepath.Join(tlsFixtures, "server.pem"),
		"--tls-key", filepath.Join(tlsFixtures, "server.key"))
	t.Cleanup(stop)
	return addr
}

func TestTLSRoundTrip(t *testing.T) {
	addr := startTLSServer(t)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("s3cret"), WithTLS(trusting(t, "ca.pem")))
	if err != nil {
		t.Fatalf("Dial over TLS: %v", err)
	}
	defer c.Close()
	if err := c.Initialize(ctx); err != nil {
		t.Fatalf("Initialize over TLS: %v", err)
	}
	got, err := c.Remember(ctx, RememberParams{TypeName: "note", Properties: map[string]any{"n": 7}})
	if err != nil {
		t.Fatalf("Remember over TLS: %v", err)
	}
	ko, err := c.Get(ctx, got.KOID, "")
	if err != nil {
		t.Fatalf("Get over TLS: %v", err)
	}
	if n, _ := ko.Properties["n"].(float64); n != 7 {
		t.Fatalf("Get properties wrong: %+v", ko.Properties)
	}
}

func TestTLSUntrustedServerRefused(t *testing.T) {
	addr := startTLSServer(t)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c, err := Dial(ctx, addr, WithToken("s3cret"), WithTLS(trusting(t, "other-ca.pem")))
	if err == nil {
		c.Close()
		t.Fatal("a server outside the trust anchors was accepted")
	}
}
