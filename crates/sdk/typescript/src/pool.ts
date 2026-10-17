// The §3.4 connection pool: a client-side abstraction over dial (no
// protocol surface — api-v1.json is frozen). The factory is the seam:
// each call must return one fully established session (dialed +
// initialized), which is also the auth reset on every reconnect. Release
// rolls back any open transaction (session reset — the next borrower
// never inherits it, §21), reaps lifetime-expired connections, and hands
// the connection to a waiter; acquire waits up to acquireTimeoutMs and
// throws the retryable RESOURCE_EXHAUSTED when the pool stays exhausted.
// A failed health ping drops the connection and dials a fresh one; a dead
// server makes the dial path retry until the deadline (the reconnect leg
// of §21). Mirrors crates/sdk/go/pool.go.
//
// ponytail: one idle stack and a FIFO waiter queue (wake one waiter per
// return, not Go's broadcast herd) — per-waiter priorities only if a
// profile ever shows contention.

import type { Client } from "./client.ts";
import { withDeadline } from "./client.ts";
import { McpError } from "./error.ts";
import type { Tx } from "./tx.ts";

export interface PoolConfig {
  /** Returns one fully established session per call. */
  factory: () => Promise<Client>;
  /** Bounds the pool (default 10). */
  maxConns?: number;
  /** Honored by fillMinIdle (default 0; no background refill). */
  minIdle?: number;
  /** Bounds a waiter (default 30s; RESOURCE_EXHAUSTED). */
  acquireTimeoutMs?: number;
  /** Reaps a connection idle longer than this (default 60s). */
  idleTimeoutMs?: number;
  /** Reaps a connection older than this (default 1h). */
  maxLifetimeMs?: number;
  /** Pings a borrowed connection when it sat idle longer than this
   * (default 30s; a near-zero value pings every borrow). */
  healthCheckIntervalMs?: number;
}

export interface PoolStats {
  total: number;
  idle: number;
}

interface Waiter {
  resolve: () => void;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** One checked-out connection. Client calls go through client(); begin
 * pins the transaction so release can reset the session. */
export class PooledConn {
  private p: Pool;
  private c: Client;
  private tx: Tx | null = null;
  createdAt: number;
  lastUsed: number;
  broken = false;

  constructor(p: Pool, c: Client, now: number) {
    this.p = p;
    this.c = c;
    this.createdAt = now;
    this.lastUsed = now;
  }

  /** The underlying client (transport-level access). */
  client(): Client {
    return this.c;
  }

  /** Opens a transaction pinned to this connection; release rolls it back
   * if it is still open. */
  async begin(txnId?: string): Promise<Tx> {
    const tx = await this.c.begin(txnId);
    this.tx = tx;
    return tx;
  }

  /** Returns the connection to the pool, rolling back a still-open
   * transaction first (session reset). */
  async release(): Promise<void> {
    if (this.tx !== null && !this.tx.done()) {
      try {
        await withDeadline(5000, (signal) => this.tx!.rollback({ signal }));
      } catch {
        this.broken = true; // session reset failed — drop the connection
      }
    }
    this.tx = null;
    this.p.put(this);
  }

  /** Drops the connection (for a caller that saw a transport error and
   * does not trust the connection anymore). */
  invalidate(): void {
    this.broken = true;
    this.p.put(this);
  }
}

/** A bounded pool of client connections. */
export class Pool {
  private cfg: Required<Omit<PoolConfig, "factory">> & Pick<PoolConfig, "factory">;
  private idle: PooledConn[] = [];
  private total = 0;
  private waiters: Waiter[] = [];
  private closedFlag = false;

  /** Builds a pool with the defaults filled in. */
  constructor(cfg: PoolConfig) {
    if (!cfg.factory) throw new Error("aikoql: PoolConfig.factory is required");
    this.cfg = {
      factory: cfg.factory,
      maxConns: cfg.maxConns ?? 10,
      minIdle: cfg.minIdle ?? 0,
      acquireTimeoutMs: cfg.acquireTimeoutMs ?? 30_000,
      idleTimeoutMs: cfg.idleTimeoutMs ?? 60_000,
      maxLifetimeMs: cfg.maxLifetimeMs ?? 3_600_000,
      healthCheckIntervalMs: cfg.healthCheckIntervalMs ?? 30_000,
    };
  }

  /** Returns a connection: an idle one (health-checked when due), a fresh
   * dial, or — when the pool is exhausted — it waits for a return up to
   * acquireTimeoutMs and fails with the retryable RESOURCE_EXHAUSTED. */
  async acquire(): Promise<PooledConn> {
    for (;;) {
      if (this.closedFlag) {
        throw new McpError(
          "UNAVAILABLE",
          "connection pool is closed",
          true,
          "Create a new pool.",
        );
      }
      while (this.idle.length > 0) {
        const pc = this.idle.pop()!;
        if (this.expired(pc)) {
          this.total--;
          await pc.client().close().catch(() => {});
          continue;
        }
        const now = Date.now();
        if (now - pc.lastUsed > this.cfg.healthCheckIntervalMs) {
          // Ping: a dead connection is dropped and replaced by a fresh
          // dial (the reconnect leg of §21).
          try {
            await pc.client().health();
          } catch {
            this.total--;
            await pc.client().close().catch(() => {});
            continue;
          }
          pc.lastUsed = now;
          return pc;
        }
        pc.lastUsed = now;
        return pc;
      }
      if (this.total < this.cfg.maxConns) {
        this.total++;
        let c: Client;
        try {
          c = await this.dialRetry();
        } catch (e) {
          this.total--;
          throw e;
        }
        const now = Date.now();
        return new PooledConn(this, c, now);
      }
      // Exhausted: wait for a return or the acquire timeout.
      let entry!: Waiter;
      const wake = new Promise<"wake">((resolve) => {
        entry = { resolve: () => resolve("wake") };
        this.waiters.push(entry);
      });
      const timer = new Promise<"timer">((resolve) => {
        const t = setTimeout(() => resolve("timer"), this.cfg.acquireTimeoutMs);
        t.unref?.();
      });
      if ((await Promise.race([wake, timer])) === "timer") {
        const i = this.waiters.indexOf(entry);
        if (i >= 0) this.waiters.splice(i, 1);
        throw new McpError(
          "RESOURCE_EXHAUSTED",
          "connection pool exhausted: all connections busy and the acquire timeout elapsed",
          true,
          "Retry with backoff, or raise MaxConns.",
        );
      }
    }
  }

  /** Calls the factory with a short backoff until the deadline — the
   * reconnect leg: a dead server keeps the borrow pending and succeeds
   * once the server returns. */
  private async dialRetry(): Promise<Client> {
    const deadline = Date.now() + this.cfg.acquireTimeoutMs;
    let lastErr: unknown;
    for (;;) {
      try {
        return await this.cfg.factory();
      } catch (e) {
        lastErr = e;
      }
      if (Date.now() >= deadline) {
        throw new McpError(
          "UNAVAILABLE",
          `could not connect to the server: ${String(lastErr)}`,
          true,
          "Verify the server; the pool retries until the acquire timeout.",
        );
      }
      await sleep(100);
    }
  }

  /** Whether an idle connection was reaped by the idle or lifetime
   * timeout. */
  private expired(pc: PooledConn): boolean {
    const now = Date.now();
    return (
      now - pc.lastUsed > this.cfg.idleTimeoutMs ||
      now - pc.createdAt > this.cfg.maxLifetimeMs
    );
  }

  /** Returns a connection to the pool: to a waiter when one is blocked,
   * else to the idle stack; lifetime-expired or broken connections are
   * dropped instead. Public only for PooledConn — the stand-in for Go's
   * same-package access. */
  put(pc: PooledConn): void {
    const now = Date.now();
    pc.lastUsed = now;
    const drop =
      this.closedFlag || pc.broken || now - pc.createdAt > this.cfg.maxLifetimeMs;
    if (!drop) {
      this.idle.push(pc);
      const w = this.waiters.shift();
      if (w) w.resolve();
    } else {
      this.total--;
    }
    if (drop) {
      void pc.client().close().catch(() => {});
    }
  }

  /** Dials until minIdle connections sit idle (best-effort, no background
   * refill). */
  async fillMinIdle(): Promise<void> {
    for (;;) {
      if (
        this.closedFlag ||
        this.idle.length >= this.cfg.minIdle ||
        this.total >= this.cfg.maxConns
      ) {
        return;
      }
      this.total++;
      let c: Client;
      try {
        c = await this.dialRetry();
      } catch (e) {
        this.total--;
        throw e;
      }
      this.put(new PooledConn(this, c, Date.now()));
    }
  }

  /** The observable pool shape. */
  stats(): PoolStats {
    return { total: this.total, idle: this.idle.length };
  }

  /** Closes the idle connections and marks the pool closed; checked-out
   * connections are closed when they are released. */
  async close(): Promise<void> {
    if (this.closedFlag) return;
    this.closedFlag = true;
    const idle = this.idle;
    this.idle = [];
    this.total -= idle.length;
    for (const pc of idle) {
      await pc.client().close().catch(() => {});
    }
  }
}
