// Typed parameter/result shapes for the canonical Database API surface
// over the MCP tools. The server's tool registry is the schema source
// (crates/sdk/go/tools.go mirrors it); everything is snake_case on the
// wire, matching the frozen vectors.

/** Establishes session identity (MRFC-0040). */
export interface SessionParams {
  agent_id?: string;
  run_id?: string;
  tenant?: string;
  roles?: string[];
}

/** One knowledge-object commit (create or new version). */
export interface RememberParams {
  subject?: string;
  type_name: string;
  koid?: string;
  properties?: Record<string, unknown>;
  note?: string;
  idempotency_key?: string;
  embed?: boolean;
  expected_version?: number;
  retention_ms?: number;
  semantic?: Record<string, unknown>;
}

/** The server's answer to remember. */
export interface Remembered {
  koid: string;
  version: number;
  commit_ts: number;
}

/** A fetched KO (the wire shape — the canonical subset of the kernel's). */
export interface KnowledgeObject {
  koid: string;
  version: number;
  state?: string;
  properties: Record<string, unknown>;
}

/** A hybrid recall query (vector + text + filters, RRF/weighted fusion). */
export interface FindSimilarParams {
  subject?: string;
  text?: string;
  vector?: number[];
  type_name?: string;
  k?: number;
  fusion?: string;
  embedding_model?: string;
  wait_for_freshness_ms?: number;
}

/** One recall hit. */
export interface ScoredKO {
  koid: string;
  score: number;
  type_name: string;
}

/** The server's metrics payload. */
export interface Metrics {
  journal_seq: number;
  total_objects: number;
  active_objects: number;
  uptime_seconds: number;
  by_lifecycle?: Record<string, unknown>;
  by_type?: Record<string, unknown>;
}
