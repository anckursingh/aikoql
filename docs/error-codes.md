# Error Code Contract

Contract version: 1

The machine-readable error surface of `aikoql-mcp`. Every error an agent can
receive is one of the codes below — numbered wire codes on the JSON-RPC
frame, MRFC-0040 envelope codes inside tool results, kernel display tags as
message-text prefixes, and one SDK-side code raised client-side.

This file is the contract: it is pinned in both directions by
`crates/services/api/mcp/tests/cli_contract.rs` (cl01a needle pin, cl01b
source→doc completeness — a new code literal in the protocol source that is
not documented here turns that test RED).

## Wire codes

JSON-RPC 2.0 `error.code` on TCP/stdio frames. All server-produced.

| Code | Meaning | Produced by |
|------|---------|-------------|
| -32700 | Parse error: the frame is not valid JSON-RPC | `transport.rs` frame reader |
| -32601 | Method not found | `dispatcher.rs` unknown-method arm (cl01c) |
| -32602 | Invalid params: missing/unknown arguments, malformed koid, parser guard | `dispatcher.rs` tool/txn arms, `tools/txn.rs` |
| -32603 | Internal error: tool failure or subscription ack of an unknown id — the message carries the kernel tag | `dispatcher.rs` `notifications/ack` of a never-subscribed id (cl01d) |
| -32000 | Connection cap exceeded or 1 MiB frame cap overflow | `transport.rs` P5-M11 guards |
| -32001 | Untrusted TCP session: no roles assigned, invalid or missing token | `authz.rs`, `transport.rs` initialize guard |
| -32002 | Request timeout: the request worker exceeded `request_timeout_ms` | `transport.rs` worker (P5-M11) |
| -32004 | Role→tool enforcement denied (PRR-2) | `tool_registry.rs` — defense in depth, no reachable producer: `serve` refuses a role-less `--tcp-token` at startup with exit 2 before any request can be routed (cl01e) |

## Envelope codes

MRFC-0040 structured errors: the `error.code` string inside a tool result
(`wrap_result`), classified from the message text by
`ErrorCode::classify`. Retryable codes carry `retryable: true`.

| Code | Retryable | Classifier keywords | Produced by |
|------|-----------|---------------------|-------------|
| ACCESS_DENIED | no | "access denied", "unauthorized", "login required" | ACL refusal (sv-suite) |
| VERSION_CONFLICT | yes | "version conflict", "conflict" | OCC write at a stale version (sv-suite) |
| NOT_FOUND | no | "not_found", "not found", "notfound" | get of an unknown KOID (cl01f) |
| VALIDATION_ERROR | no | "missing", "invalid", "bad" | get without a koid (cl01f) |
| RATE_LIMITED | yes | "rate", "too many" | rate-limit refusal |
| TIMEOUT | yes | "timeout", "timed out" | operation timeout |
| INTERNAL | no | fallthrough | txn_commit of a never-opened txn (cl01f) |
| NOT_A_PROGRAM | no | "not a program" | program tool on a non-program KOID |
| COMPILE_ERROR | no | "compile", "parse", "syntax", "aikoql1" | aikoql parse errors, `AIKOQL1xxx` prefix (cl01f) |

RATE_LIMITED, TIMEOUT, VERSION_CONFLICT and NOT_A_PROGRAM have no dedicated
producer pin in cli_contract.rs: their producers need config- or
timing-dependent harnesses. The sv-suite and test_mcp_client.py exercise them
on the happy paths of their subsystems.

## Kernel error tags

`KError` display prefixes. These are message text, not codes — they ride the
message of a wire or envelope error and are classified by the envelope table
above (e.g. `NOT_FOUND: <koid>` → NOT_FOUND via the "not_found" keyword).

| Tag | Meaning |
|-----|---------|
| INVALID_OBJECT | malformed KOID or object payload |
| INVALID_SCHEMA | schema constraint violation |
| INVALID_QUERY | aikoql parse or semantic error |
| VERSION_CONFLICT | OCC expected/found version mismatch |
| ACCESS_DENIED | owner-default ACL refusal |
| INVALID_STATE | illegal state transition |
| INVALID_EPISTEMIC | illegal epistemic transition |
| NOT_FOUND | unknown KOID or index entry |
| UNSUPPORTED_OPERATION | operation outside the current capability set |
| CANCELLED | streaming plan cancelled mid-scan (P5-M4) |
| INDEX_LAG_EXCEEDED | index behind the journal |
| JOB_REJECTED | async job refused |
| STORE | storage engine error |
| CODEC | serialization error |

## SDK-side codes

Raised by the Python SDK client-side; never sent by the server.

| Code | Meaning | Produced by |
|------|---------|-------------|
| VERSION_MISMATCH | `initialize` failed fast: `serverInfo.version` older than `MIN_SERVER_VERSION` | `python/aikoql/mcp_client.py` (test_version_contract.py test_sdk_rejects_old_server) |
