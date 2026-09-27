// Command web-service is the "other application" proof for the Go SDK: a
// small HTTP API in front of an aikoql-mcp server, exercising remember,
// get, query, and health through one pooled Client.
//
//	go run . -addr 127.0.0.1:8080 -token s3cret -db ./kb -bin ../target/release/aikoql-mcp
//	curl -X POST localhost:8080/remember -d '{"type_name":"person","properties":{"name":"ada"}}'
//	curl localhost:8080/query?q=MATCH%20person%20RETURN%20*
//	curl localhost:8080/health
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/exec"
	"sync"
	"time"

	"github.com/ancku/aikoql-sdk"
)

func main() {
	var (
		addr        = flag.String("addr", "127.0.0.1:8080", "HTTP listen address")
		db          = flag.String("db", "./kb", "aikoql-mcp database directory")
		token       = flag.String("token", "s3cret", "client token (the TOKEN segment of the server spec)")
		serverToken = flag.String("server-token", "s3cret:acme:admin", "full --tcp-token spec for the spawned server (TOKEN[:TENANT[:ROLE1,ROLE2]])")
		bin         = flag.String("bin", "", "aikoql-mcp binary to spawn (skip to use an already-running server)")
		mcp         = flag.String("mcp", "127.0.0.1:9090", "aikoql-mcp --listen address")
	)
	flag.Parse()

	if *bin != "" {
		stop, err := spawn(*bin, *mcp, *db, *serverToken)
		if err != nil {
			log.Fatalf("spawn: %v", err)
		}
		defer stop()
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	client, err := aikoql.Dial(ctx, *mcp, aikoql.WithToken(*token), aikoql.WithClientInfo("web-service-example", "0.0.0"))
	if err != nil {
		log.Fatalf("dial: %v", err)
	}
	defer client.Close()
	if err := client.Initialize(ctx); err != nil {
		log.Fatalf("initialize: %v", err)
	}

	var mu sync.Mutex // serializes HTTP requests over the one connection
	call := func(fn func(*aikoql.Client, *http.Request) (any, error)) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			mu.Lock()
			defer mu.Unlock()
			body, err := fn(client, r)
			if err != nil {
				http.Error(w, err.Error(), http.StatusBadGateway)
				return
			}
			w.Header().Set("Content-Type", "application/json")
			if err := json.NewEncoder(w).Encode(body); err != nil {
				log.Printf("encode response: %v", err)
			}
		}
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /remember", call(func(c *aikoql.Client, r *http.Request) (any, error) {
		var p aikoql.RememberParams
		if err := json.NewDecoder(r.Body).Decode(&p); err != nil {
			return nil, fmt.Errorf("decode body: %w", err)
		}
		return c.Remember(r.Context(), p)
	}))
	mux.HandleFunc("GET /ko/{koid}", call(func(c *aikoql.Client, r *http.Request) (any, error) {
		return c.Get(r.Context(), r.PathValue("koid"), "")
	}))
	mux.HandleFunc("GET /query", call(func(c *aikoql.Client, r *http.Request) (any, error) {
		var out any
		raw, err := c.Aikoql(r.Context(), r.URL.Query().Get("q"), "")
		if err != nil {
			return nil, err
		}
		if err := json.Unmarshal(raw, &out); err != nil {
			return nil, fmt.Errorf("query result: %w", err)
		}
		return out, nil
	}))
	mux.HandleFunc("GET /health", call(func(c *aikoql.Client, r *http.Request) (any, error) {
		var out any
		raw, err := c.Health(r.Context())
		if err != nil {
			return nil, err
		}
		if err := json.Unmarshal(raw, &out); err != nil {
			return nil, fmt.Errorf("health payload: %w", err)
		}
		return out, nil
	}))

	log.Printf("listening on http://%s (mcp %s)", *addr, *mcp)
	log.Fatal(http.ListenAndServe(*addr, mux))
}

// spawn starts an aikoql-mcp server and returns a stop func. Readiness is
// the server accepting TCP connections.
func spawn(bin, listen, db, token string) (stop func(), err error) {
	args := []string{"serve", "--listen", listen, "--tcp-token", token, db}
	cmd := exec.Command(bin, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start %s: %w", bin, err)
	}
	stop = func() {
		if err := cmd.Process.Kill(); err != nil {
			log.Printf("kill mcp server: %v", err)
		}
		if err := cmd.Wait(); err != nil {
			log.Printf("wait mcp server: %v", err)
		}
	}
	time.Sleep(500 * time.Millisecond) // the server binds fast; the SDK dial retries anyway
	return stop, nil
}
