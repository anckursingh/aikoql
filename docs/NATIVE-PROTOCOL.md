# The aikoql native protocol (D-15)

A framed binary transport for the database surface. MCP JSON-RPC remains a
first-class agent-integration surface; the native protocol is the database
protocol — the SDK API does not change when the transport changes. Every
message class funnels into the same server seams as MCP (`call_tool`,
`execute_stream_query`, `tool_session_init`), so there is exactly one
implementation of the database surface.

- **Wire**: `crates/protocol/native` — a zero-dependency codec crate.
- **Server**: `crates/services/api/mcp/src/native.rs` — a second TCP
  listener (`--native-port`, default `127.0.0.1:9070`, loopback-only), same
  lifecycle as the MCP listener (CAS connection cap, shutdown drain, shared
  rate limiter).
- **SDK**: `Client::connect_native` in `crates/sdk/rust` — the §6 oracle
  (`crates/sdk/rust/tests/native.rs`) freezes the behavior matrix.

## Wire format (§6)

| field | bytes | encoding |
|---|---|---|
| magic | 4 | `AKQL` |
| protocol_version | 2 | u16 BE — `1` |
| flags | 2 | u16 BE — bit 0 = response |
| request_id | 8 | u64 BE — client-assigned, monotonic per connection |
| msg_type | 2 | u16 BE |
| payload_length | 4 | u32 BE — JSON UTF-8 payload |
| payload | n | JSON |
| checksum | 4 | crc32 u32 **LE**, IEEE poly `0xEDB88320`, over everything before it |

Header = 22 bytes. `MAX_PAYLOAD` = 1 MiB.

Message classes: `HELLO=1 AUTH=2 PING=3 BEGIN=4 COMMIT=5 ROLLBACK=6
PREPARE=7 EXECUTE=8 QUERY=9 QUERY_CHUNK=10 QUERY_END=11 CANCEL=12 CLOSE=13
ERROR=14`.

## The 12 invariants

1. Every frame carries the magic and the version; a mismatch is explicit
   (`VERSION_MISMATCH`), never silently tolerated.
2. Every response echoes exactly one request's `request_id` (FLAG_RESPONSE).
3. A `request_id` at or below the highest seen is stale — the client skips
   it; a higher id is a protocol error (mirrors MCP §3.3 correlation).
4. A truncated frame closes the connection fast.
5. An oversized payload (length cap / claimed-vs-actual) closes the
   connection fast.
6. A checksum mismatch closes the connection fast.
7. HELLO is always allowed — negotiation precedes everything.
8. A HELLO version mismatch is an ERROR frame, then the connection drops.
9. Only AUTH before authentication; everything else is refused
   (`NOT_AUTHENTICATED`). A bad token is `AUTHENTICATION_FAILED` but the
   connection **survives** for a retry.
10. Cancellation is observable: CANCEL (fire-and-forget, names the target
    `request_id`) sets a per-connection flag the stream pump checks between
    chunks; QUERY_END records `cancelled: true` + the chunks actually sent.
11. Streaming QUERY answers: one QUERY head frame (`stream_id`, `chunk: 0`,
    `total_chunks`, first rows), then QUERY_CHUNK frames (`chunk`, `done`),
    then exactly one QUERY_END. A non-stream QUERY answers in one QUERY
    frame with every row; over the frame cap it is `FRAME_TOO_LARGE` —
    use `stream: true`.
12. Every decode failure and unknown message type fails safely — an ERROR
    frame or a drop, never a hang or a crash; the server survives and keeps
    serving other connections.

## Lifecycle

1. **HELLO** → `{protocol_version, server_version, capabilities, session_id}`.
2. **AUTH** `{token}` → identity comes exclusively from a verified
   `--tcp-token` (PRR-2); roles/tenant flow into the session.
3. **EXECUTE** `{tool, args}` → the whole tool surface in one message class
   through `call_tool`; the response is the `{ok, data}` envelope, failures
   `{ok: false, error: {code, message}}`.
4. **PREPARE** `{aikoql}` → validation-only compile; `{prepared, error?}`.
5. **QUERY** `{query, stream?, subject?}` → invariant 11.
6. **BEGIN/COMMIT/ROLLBACK** `{txn_id}` → the txn classes funnel into
   `txn_dispatch`; handles are connection-scoped (a cross-connection stage
   or commit fails `INTERNAL`).
7. **PING** → `{pong: true}`; **CANCEL** → no response; **CLOSE** → `{ok}`
   then the server drops the connection.

Error codes: `VERSION_MISMATCH`, `AUTHENTICATION_FAILED`,
`NOT_AUTHENTICATED`, `CONNECTION_LIMIT`, `RATE_LIMITED`, `FRAME_TOO_LARGE`,
`QUERY_FAILED`, `INTERNAL` (txn errors — the oracle pins this code).

## The oracle

`crates/sdk/rust/tests/native.rs` (14 legs, serial): HELLO version gate,
auth-before-ops + retry-after-bad-token, unknown/oversized/truncated/corrupt
frames drop the connection fast and the server survives, PREPARE/EXECUTE/
QUERY surface, the raw txn lifecycle, cancellation observable mid-stream,
per-connection txn state, mid-stream server death surfaces as a client
stream error, the SDK legs over `connect_native`, and the shared
conformance runner pin (`sdk-conformance --language rust --transport
native`).
