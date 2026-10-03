# aikoql — Go SDK

The first-party Go client for the [aikoql](https://github.com/anckursingh/aikoql) knowledge database: the canonical Database API (remember, get, query, traverse, hybrid search, context compile, prepare/execute) over the MCP transport, with a pooled client and stdio dialing.

```go
import "github.com/anckursingh/aikoql/sdk/go"

client, err := aikoql.Dial(ctx, "127.0.0.1:9090", aikoql.WithToken("s3cret"))
defer client.Close()
```

`DialStdio` spawns and talks to an `aikoql-mcp` binary directly (the docker `-i` contract). The [web-service example](examples/web-service) is the "other application" proof: an HTTP API on top of one pooled client.

- Tests: `AIKOQL_MCP_BIN=$PWD/target/release/aikoql-mcp go test -v ./...`
- Fuzz smokes (8 targets): `go test -fuzz '^Fuzz' -fuzztime 5s .` (one target at a time)
- License: Apache-2.0
