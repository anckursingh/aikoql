// The TypeScript SDK for Aikoql (D-13): a remote MCP client — the
// canonical Database API (docs/DATABASE-API.md) over the MCP wire, mirror
// of the Rust reference (crates/sdk/rust). Node-first: the browser
// transport is a Transport-implementation swap (§25 "where applicable");
// no embedded mode (the kernel is Rust-only).

export { Client, MIN_SERVER_VERSION, withDeadline } from "./client.ts";
export { McpError } from "./error.ts";
export { TcpTransport, type Transport } from "./tcp.ts";
export { Pool, PooledConn, type PoolConfig, type PoolStats } from "./pool.ts";
export { Tx, type StagedOp, type CommitResult } from "./tx.ts";
export type {
  FindSimilarParams,
  KnowledgeObject,
  Metrics,
  RememberParams,
  Remembered,
  ScoredKO,
  SessionParams,
} from "./tools.ts";
