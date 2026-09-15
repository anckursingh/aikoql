# Server Contract

The aikoql-mcp standalone database server (P5-M11, ND-11): a plain TCP
listener speaking line-delimited JSON-RPC 2.0, on top of the existing MCP
transport. One frame per line, `\n`-terminated. PG-wire compatibility is
explicitly out of scope.

## Startup

```
aikoql-mcp serve <db-path> \
  --listen 127.0.0.1:9090 \
  --tcp-token TOKEN[:TENANT[:ROLE1,ROLE2]] \
  [--request-timeout-secs 30] \
  [--max-connections 64]
```

- `--tcp-token` is required in TCP mode (fail-closed); at least one role per
  token spec. The token is passed as `params.token` to `initialize`.
- Loopback-only binds. `--request-timeout-secs` (default 30) bounds one
  request's wall-clock execution; `--max-connections` (default 64) caps
  concurrent client connections.

## Methods

- `initialize` — token auth; everything except `ping` is rejected with
  -32001 and the connection dropped until this succeeds.
- `ping` — liveness, no auth required.
- `koql/query` — read-only KOQL: `{"query": "MATCH Node ..."}`. Write
  statements (CREATE/UPDATE/DELETE/INGEST) are rejected with -32602. Runs
  under the request timeout; a timed-out query is cancelled and the client
  gets -32002.
- `koql/execute` — any KOQL statement (writes ack `{koid, version,
  commit_ts}`). Also runs under the request timeout.
- `tools/call` — the MCP tool surface, including the transaction tools
  `txn_begin` / `txn_stage` / `txn_commit` / `txn_rollback`. Transaction
  handles are connection-scoped: a handle opened on one connection is
  invisible on another.
- `shutdown` — graceful shutdown: replies `{"shutting_down": true}`, closes
  the connection (EOF), stops accepting, drains, and the process exits 0.

## Lifecycle

- **Graceful shutdown**: after the `shutdown` ack the accept loop stops, all
  in-flight queries are cancelled, open connections are closed as their
  handlers notice the flag, and the server exits zero. Acknowledged writes
  are durable before the ack (the kernel contract).
- **Drain**: shutdown cancels every in-flight query and then actively
  closes every remaining socket (`shutdown(Both)`), which wakes handlers
  blocked reading — connections close promptly rather than waiting for
  their next frame. The wait is still bounded by `request_timeout_secs` as
  a backstop: the process never hangs forever.
- **Request timeout**: each `koql/*` request runs in a worker thread; the
  handler waits `request_timeout_secs`, then cancels the worker and replies
  -32002. The synchronous interpreter cannot be force-killed mid-query, so
  a non-parking query finishes on its own after cancellation; the caller is
  already gone.
- **Connection limit**: at `max_connections` concurrent connections the
  next accept receives a -32000 frame and is dropped immediately
  (check-then-act: two simultaneous accepts can both pass the check — the
  cap is advisory at the margin).
- **Frames are capped at 1 MiB**: an oversized line drops the connection
  instead of growing unbounded.

## Crash windows (test hooks)

- `AIKOQL_QUERY_PARK=armed` — the FIRST `koql/query` (not `koql/execute`,
  and one-shot per process) parks before execution: it writes the
  `AIKOQL_QUERY_PARK_MARKER` file and spins until its cancellation token is
  cancelled (request timeout, shutdown drain, or client disconnect
  followed by the timeout). Later queries get normal service — a cancelled
  parked query never poisons the next client.
- `AIKOQL_QUERY_EXIT_MARKER` — written only by a query that parked AND was
  cancelled, immediately before execution resumes. Suites use it to prove
  cancellation reached the worker.
