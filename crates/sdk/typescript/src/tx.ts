// Tx implements the §3.5 transaction handle: the txn_id is a first-class
// field here — it never leaks as a bare tool argument that a caller
// threads between call sites. The four txn_* tools are reachable only
// through Tx methods. Mirrors the Go SDK's transactions.go and the Rust
// reference (crates/sdk/rust/src/tx.rs).

import type { Client } from "./client.ts";
import { McpError } from "./error.ts";

/** One staged write (the txn_stage op shape). */
export interface StagedOp {
  action: string;
  type_name?: string;
  koid?: string;
  properties?: Record<string, unknown>;
}

/** The txn_commit outcome: the staged write results and whether this
 * commit was a retry of an already-committed txn_id. */
export interface CommitResult {
  results?: unknown[];
  deduped?: boolean;
}

/**
 * A staged write handle: begin on the Client, execute stages ops, commit
 * or rollback closes it. A closed handle refuses further use with
 * INVALID_ARGUMENT.
 */
export class Tx {
  private c: Client;
  private txId: string;
  private closed = false;

  constructor(c: Client, txnId: string) {
    this.c = c;
    this.txId = txnId;
  }

  /** The txn_id (for retries — commit dedupes by it, P5-M20). */
  id(): string {
    return this.txId;
  }

  /** True once commit or rollback closed the handle (the pool's session
   * reset checks this). */
  done(): boolean {
    return this.closed;
  }

  /** Stages one write. */
  async execute(op: StagedOp, opts: { signal?: AbortSignal } = {}): Promise<void> {
    await this.step("txn_stage", { txn_id: this.txId, op }, opts);
  }

  /** Applies the staged writes and closes the handle. A commit that errors
   * leaves the handle open: the server dedupes by txn_id, so the caller
   * may retry commit or begin with the same id (P5-M20). */
  async commit(opts: { signal?: AbortSignal } = {}): Promise<CommitResult> {
    const raw = await this.step("txn_commit", { txn_id: this.txId }, opts);
    return (raw ?? {}) as CommitResult;
  }

  /** Discards the staged writes and closes the handle. */
  async rollback(opts: { signal?: AbortSignal } = {}): Promise<void> {
    await this.step("txn_rollback", { txn_id: this.txId }, opts);
  }

  private async step(
    name: string,
    args: Record<string, unknown>,
    opts: { signal?: AbortSignal },
  ): Promise<unknown> {
    if (this.closed) {
      throw McpError.invalidArgument(`transaction ${this.txId} is closed`);
    }
    const raw = await this.c.callTool(name, args, opts);
    if (name === "txn_commit" || name === "txn_rollback") {
      this.closed = true;
    }
    return raw;
  }
}
