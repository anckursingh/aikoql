# Version Compatibility Contract

Contract version: 1

The SDK↔server compatibility contract (P5-M12, ND-12). One rule, enforced
fail-fast at `initialize` time:

## The rule

The Python SDK declares `MIN_SERVER_VERSION` (a dotted-int version). On
`initialize`, the SDK reads `serverInfo.version` from the server's response
and compares it against the minimum. A server **older** than the minimum is
refused immediately with a `VERSION_MISMATCH` McpError — the connection
never gets used for real work. A server **newer** than the minimum is
accepted: the SDK is forward-compatible by construction, there is no upper
bound.

`MIN_SERVER_VERSION` must always equal the workspace version in the root
`Cargo.toml`. A future workspace bump turns
`tests/test_version_contract.py::test_min_server_version_matches_workspace`
RED by itself until the SDK constant follows — the same self-pinning pattern
as sdk001's version parity test.

## Current matrix

| SDK `MIN_SERVER_VERSION` | Server `serverInfo.version` | Result |
|--------------------------|-----------------------------|--------|
| `0.1.19` | `0.1.19` (current workspace) | accepted |
| `0.1.19` | `0.0.1` | refused: `VERSION_MISMATCH`, fail-fast |
| `0.1.19` | `0.2.0` | accepted (forward-compatible optimism) |

## Enforcement

* **SDK side**: `python/aikoql/mcp_client.py` — `initialize()` parses
  `serverInfo.version` as dotted ints (non-numeric segments compare as
  never-satisfied) and raises `McpError(code="VERSION_MISMATCH",
  retryable=False)` with an upgrade suggestion. Pinned by
  `tests/test_version_contract.py::test_sdk_rejects_old_server` (a fake
  server replying `0.0.1`) and `test_sdk_accepts_current_server` (the
  repo-built binary passing the check).
* **Server side**: `aikoql-mcp` reports `serverInfo.version =
  env!("CARGO_PKG_VERSION")` on every `initialize` response
  (`dispatcher.rs`) — the version the SDK reads. The ABI it checks is
  surfaced by the `abi_version` tool (pinned by cl03a status).

## Changelog

* **Contract version: 1** (2026-09-15, P5-M12): minimum-version check added
  to the SDK. Prior SDKs did no version validation — the matrix above is
  the new fail-fast behavior.
