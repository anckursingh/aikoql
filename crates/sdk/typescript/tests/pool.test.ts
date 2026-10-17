// The §3.4 pool legs: ports of the Go suite (crates/sdk/go/pool_test.go).
// The scripted-server legs pin the pool mechanics (exhaustion →
// RESOURCE_EXHAUSTED, transaction pinning + session reset on release,
// reuse after a cancelled call, min-idle fill); the real-server leg
// (skipped without AIKOQL_MCP_BIN) pins reconnect and auth reset across a
// server restart. The factory is the seam: each call must return a fully
// established session for one connection.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { Client, McpError, Pool } from "../src/index.ts";
import { scripted, sleep, toolResult, type Frame } from "./helpers.ts";

interface TxnCall {
  name: string;
  args: Record<string, unknown>;
}

/** Answers every tool a pooled connection can issue and logs the call. */
function poolResponder(log: TxnCall[]) {
  return (req: Record<string, unknown>): Frame[] => {
    const params = req["params"] as { name?: string; arguments?: Record<string, unknown> };
    const call: TxnCall = { name: params.name ?? "", args: params.arguments ?? {} };
    log.push(call);
    let data: unknown;
    switch (call.name) {
      case "health":
        data = { status: "ok" };
        break;
      case "aikoql":
        data = { results: [] };
        break;
      case "txn_begin":
        data = { txn_id: call.args["txn_id"], snapshot_ts: 1000 };
        break;
      case "txn_stage":
        data = { staged: 1 };
        break;
      case "txn_rollback":
        data = { rolled_back: true };
        break;
      default:
        throw new Error(`unexpected tool ${call.name}`);
    }
    return [[toolResult(req["id"], data), 0]];
  };
}

/** A factory that dials a fresh scripted server per call, and its dial
 * counter. */
function poolFactory(t: TestContext, respond: (req: Record<string, unknown>) => Frame[]) {
  let n = 0;
  return {
    factory: async (): Promise<Client> => {
      const addr = await scripted(t, respond);
      n += 1;
      return Client.dial(addr);
    },
    dials: () => n,
  };
}

test("exhaustion waits then RESOURCE_EXHAUSTED; a freed connection goes to a waiter", async (t) => {
  const log: TxnCall[] = [];
  const { factory, dials } = poolFactory(t, poolResponder(log));
  const p = new Pool({ factory, maxConns: 2, acquireTimeoutMs: 200 });
  t.after(() => {
    void p.close();
  });
  const a = await p.acquire();
  const b = await p.acquire();
  assert.equal(dials(), 2);
  const start = Date.now();
  await assert.rejects(
    p.acquire(),
    (e: unknown) => isCode(e, "RESOURCE_EXHAUSTED") && (e as McpError).retryable,
  );
  assert.ok(
    Date.now() - start >= 190,
    `acquire must wait for the timeout before failing, waited ${Date.now() - start}ms`,
  );
  assert.equal(dials(), 2, "an exhausted pool must not dial");
  // A freed connection is handed to a waiter without a redial.
  const waiter = p.acquire();
  await sleep(50);
  await a.release();
  const c = await waiter;
  assert.equal(dials(), 2, "handoff must not dial");
  await c.release();
  await b.release();
});

test("release rolls back the open transaction and the next borrower starts clean", async (t) => {
  const log: TxnCall[] = [];
  const { factory, dials } = poolFactory(t, poolResponder(log));
  const p = new Pool({ factory, maxConns: 1 });
  t.after(() => {
    void p.close();
  });
  const pc = await p.acquire();
  const tx = await pc.begin("abc");
  await tx.execute({ action: "create", type_name: "person", properties: { name: "ada" } });
  await pc.release();
  // Session reset: the open transaction was rolled back on release.
  const rolledBack = log.some(
    (call) => call.name === "txn_rollback" && call.args["txn_id"] === "abc",
  );
  assert.ok(rolledBack, `release must roll back the open transaction, log: ${JSON.stringify(log)}`);
  // The next borrower must NOT inherit the transaction (§21).
  const pc2 = await p.acquire();
  assert.equal(dials(), 1, "the connection must be reused");
  const tx2 = await pc2.begin();
  assert.notEqual(tx2.id(), "abc", "the next borrower must not inherit the transaction");
  assert.equal(tx2.id().length, 32, "a fresh transaction id is 32 hex chars");
  await pc2.release();
});

test("the connection is reusable after a cancelled call", async (t) => {
  const log: TxnCall[] = [];
  const { factory, dials } = poolFactory(t, poolResponder(log));
  const p = new Pool({ factory, maxConns: 1 });
  t.after(() => {
    void p.close();
  });
  const pc = await p.acquire();
  const ctrl = new AbortController();
  ctrl.abort(); // a pre-aborted signal bounds the call
  await assert.rejects(
    pc.client().callTool("aikoql", { query: "MATCH person RETURN *" }, { signal: ctrl.signal }),
    (e: unknown) => isCode(e, "TIMEOUT"),
  );
  await pc.release();
  // The connection must be reusable (§21): same conn, no redial. The stale
  // answer to the aborted call is skipped by id correlation.
  const pc2 = await p.acquire();
  assert.equal(dials(), 1, "the connection must be reused");
  const raw = await pc2.client().aikoql("MATCH person RETURN *", "");
  assert.deepEqual(raw, { results: [] });
  await pc2.release();
});

test("fillMinIdle dials to the floor", async (t) => {
  const log: TxnCall[] = [];
  const { factory, dials } = poolFactory(t, poolResponder(log));
  const p = new Pool({ factory, maxConns: 3, minIdle: 2 });
  t.after(() => {
    void p.close();
  });
  await p.fillMinIdle();
  assert.deepEqual(p.stats(), { total: 2, idle: 2 });
  assert.equal(dials(), 2, "fill must dial twice");
});

/** Spawns the binary on a fixed addr with a fixed db dir and waits for
 * readiness, so a test can stop and respawn on the same addr over the same
 * data. The returned stop is idempotent. */
async function startRealServerOn(bin: string, addr: string, dbDir: string): Promise<() => Promise<void>> {
  let stderr = "";
  const child: ChildProcess = spawn(
    bin,
    ["serve", "--listen", addr, "--tcp-token", "s3cret:acme:admin", dbDir],
    { stdio: ["ignore", "ignore", "pipe"], windowsHide: true },
  );
  child.stderr?.on("data", (d: Buffer) => {
    stderr += d.toString("utf8");
  });
  const [host, portStr] = addr.split(":");
  const deadline = Date.now() + 10_000;
  for (;;) {
    const up = await new Promise<boolean>((resolve) => {
      const s = net.connect(Number(portStr), host!);
      s.once("connect", () => {
        s.destroy();
        resolve(true);
      });
      s.once("error", () => resolve(false));
    });
    if (up) break;
    if (Date.now() >= deadline) {
      child.kill();
      throw new Error(`server never came up on ${addr}\nstderr:\n${stderr}`);
    }
    await sleep(100);
  }
  let stopped = false;
  return async () => {
    if (stopped) return;
    stopped = true;
    child.kill();
    await new Promise<void>((resolve) => {
      if (child.exitCode !== null) {
        resolve();
        return;
      }
      child.once("exit", () => resolve());
    });
  };
}

test(
  "reconnects after a server restart",
  {
    skip:
      process.env.AIKOQL_MCP_BIN === undefined
        ? "AIKOQL_MCP_BIN not set — real-server integration test skipped"
        : false,
  },
  async (t) => {
    const bin = process.env.AIKOQL_MCP_BIN!;
    const probe = net.createServer();
    await new Promise<void>((resolve, reject) => {
      probe.once("error", reject);
      probe.listen(0, "127.0.0.1", resolve);
    });
    const port = (probe.address() as net.AddressInfo).port;
    await new Promise<void>((resolve) => probe.close(() => resolve()));
    const addr = `127.0.0.1:${port}`;
    const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "pool-test-"));
    t.after(() => {
      void fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
    });
    const dbDir = path.join(dir, "kb");

    const stop1 = await startRealServerOn(bin, addr, dbDir);
    t.after(stop1);

    // The factory re-establishes the full session: dial + initialize with
    // the token (the auth reset on every reconnect).
    const factory = async (): Promise<Client> => {
      const c = await Client.dial(addr);
      c.withToken("s3cret");
      try {
        await c.initialize();
      } catch (e) {
        await c.close().catch(() => {});
        throw e;
      }
      return c;
    };
    // A near-zero health-check interval: every borrow pings, so a dead
    // connection is caught and replaced at checkout.
    const p = new Pool({ factory, maxConns: 1, acquireTimeoutMs: 10_000, healthCheckIntervalMs: 1 });
    t.after(() => {
      void p.close();
    });

    const pc = await p.acquire();
    await pc.client().remember({ type_name: "person", properties: { name: "ada" } });
    await pc.release();
    await stop1(); // the server dies

    // Borrow while the server is down: the pool keeps retrying the dial.
    const acquireP = p.acquire();
    const settled = await Promise.race([
      acquireP.then(() => "completed" as const),
      sleep(300).then(() => "still-waiting" as const),
    ]);
    assert.equal(settled, "still-waiting", "acquire must wait for the server to return");
    const stop2 = await startRealServerOn(bin, addr, dbDir); // the server returns
    t.after(stop2);
    const pc2 = await acquireP;
    const raw = await pc2.client().aikoql('MATCH person WHERE name == "ada" RETURN *', "");
    assert.ok(
      JSON.stringify(raw).includes("ada"),
      `the db must survive the restart: ${JSON.stringify(raw)}`,
    );
    await pc2.release();
  },
);

function isCode(e: unknown, code: string): boolean {
  return e instanceof McpError && e.code === code;
}
