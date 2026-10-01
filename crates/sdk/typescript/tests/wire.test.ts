// The wire-layer legs: ports of the Rust client's scripted-server suite
// (crates/sdk/rust/src/client.rs tests). Each test drives a fake server
// through the frozen §3.3 semantics — id correlation, deadline → TIMEOUT,
// closed client → UNAVAILABLE, the version contract, the stream frame
// protocol, the transaction guard, and self-healing after a late frame.

import assert from "node:assert/strict";
import { test } from "node:test";
import { McpError, withDeadline } from "../src/index.ts";
import { dial, respond, scripted, toolResult } from "./helpers.ts";

function isCode(e: unknown, code: string): boolean {
  return e instanceof McpError && e.code === code;
}

test("initialize sends the handshake", async (t) => {
  const reqs: Record<string, unknown>[] = [];
  const addr = await scripted(t, (req) => {
    reqs.push(req);
    return [[respond(req["id"], "0.2.0"), 0]];
  });
  const c = await dial(t, addr);
  await c.initialize();
  assert.equal(reqs.length, 1);
  assert.equal(reqs[0]!["method"], "initialize");
});

test("skips stale and id-less frames", async (t) => {
  const addr = await scripted(t, (req) => [
    ["{}", 0], // id-less push
    [JSON.stringify({ id: 0, result: {} }), 0], // stale
    [respond(req["id"], "0.2.0"), 0],
  ]);
  const c = await dial(t, addr);
  await c.initialize();
});

test("protocol error on a foreign id", async (t) => {
  const addr = await scripted(t, () => [
    [JSON.stringify({ id: 99, result: {} }), 0],
  ]);
  const c = await dial(t, addr);
  await assert.rejects(c.initialize(), (e: unknown) => isCode(e, "PROTOCOL_ERROR"));
});

test("rpc error envelope", async (t) => {
  const addr = await scripted(t, (req) => [
    [JSON.stringify({ id: req["id"], error: { code: "NOT_FOUND", message: "nope" } }), 0],
  ]);
  const c = await dial(t, addr);
  await assert.rejects(c.initialize(), (e: unknown) => isCode(e, "NOT_FOUND"));
});

test("noise frames are skipped", async (t) => {
  const addr = await scripted(t, (req) => [
    ["not json", 0],
    [respond(req["id"], "0.2.0"), 0],
  ]);
  const c = await dial(t, addr);
  await c.initialize();
});

test("a deadline maps to the retryable TIMEOUT", async (t) => {
  const addr = await scripted(t, () => []); // accepts, never answers
  const c = await dial(t, addr);
  await assert.rejects(
    withDeadline(200, (signal) => c.initialize({ signal })),
    (e: unknown) => isCode(e, "TIMEOUT") && (e as McpError).retryable,
  );
});

test("a closed client is UNAVAILABLE", async (t) => {
  const addr = await scripted(t, () => []);
  const c = await dial(t, addr);
  await c.close();
  await assert.rejects(c.callTool("health"), (e: unknown) => isCode(e, "UNAVAILABLE"));
});

test("a too-old server fails fast with VERSION_MISMATCH", async (t) => {
  const addr = await scripted(t, (req) => [[respond(req["id"], "0.0.1"), 0]]);
  const c = await dial(t, addr);
  await assert.rejects(c.initialize(), (e: unknown) => isCode(e, "VERSION_MISMATCH"));
});

test("a single-chunk stream (total_chunks 1) yields the head and ends", async (t) => {
  const addr = await scripted(t, (req) => [
    [
      JSON.stringify({
        id: req["id"],
        result: { stream_id: "s1", total_chunks: 1, results: [{ koid: "k1" }] },
      }),
      0,
    ],
  ]);
  const c = await dial(t, addr);
  const chunks: unknown[] = [];
  for await (const chunk of c.queryStream("MATCH p RETURN *", "")) chunks.push(chunk);
  assert.equal(chunks.length, 1);
  const head = chunks[0] as { results?: Array<{ koid?: string }> };
  assert.equal(head.results?.[0]?.koid, "k1");
});

test("a multi-chunk stream ends on done", async (t) => {
  const addr = await scripted(t, (req) => [
    [
      JSON.stringify({
        id: req["id"],
        result: { stream_id: "s1", total_chunks: 2, results: [{ koid: "k1" }] },
      }),
      0,
    ],
    [
      JSON.stringify({
        method: "notifications/notify",
        params: { stream_id: "s1", chunk: 2, done: true, results: [{ koid: "k2" }] },
      }),
      0,
    ],
  ]);
  const c = await dial(t, addr);
  const chunks: unknown[] = [];
  for await (const chunk of c.queryStream("MATCH p RETURN *", "")) chunks.push(chunk);
  assert.equal(chunks.length, 2);
  const second = chunks[1] as { results?: Array<{ koid?: string }> };
  assert.equal(second.results?.[0]?.koid, "k2");
});

test("an aborted stream releases the connection", async (t) => {
  const addr = await scripted(t, (req) => {
    if (req["method"] === "aikoql/stream") {
      return [
        [JSON.stringify({ id: req["id"], result: { stream_id: "s1", total_chunks: 2, results: [] } }), 0],
      ];
    }
    return [[toolResult(req["id"], { status: "healthy" }), 0]];
  });
  const c = await dial(t, addr);
  const ctrl = new AbortController();
  const stream = c.queryStream("MATCH p RETURN *", "", { signal: ctrl.signal });
  const first = await stream.next();
  assert.equal((first.value as { stream_id?: string }).stream_id, "s1");
  // Cancellation while blocked on a read goes through the abort signal
  // (return() waits for the pending read to settle — the single-threaded
  // stand-in for the Rust select on the dropped receiver). The abort
  // surfaces as the frozen TIMEOUT and the finally releases the lock.
  ctrl.abort();
  await assert.rejects(stream.next(), (e: unknown) => isCode(e, "TIMEOUT"));
  const health = await c.callTool("health");
  assert.equal((health as { status?: string }).status, "healthy");
});

test("a closed transaction handle refuses further use", async (t) => {
  const addr = await scripted(t, (req) => {
    const name = ((req["params"] as Record<string, unknown> | undefined)?.["name"]) as
      | string
      | undefined;
    switch (name) {
      case "txn_begin":
        return [[toolResult(req["id"], {}), 0]];
      case "txn_commit":
        return [[toolResult(req["id"], { results: [], deduped: false }), 0]];
      default:
        throw new Error(`unexpected tool call ${name}`);
    }
  });
  const c = await dial(t, addr);
  const tx = await c.begin();
  const res = await tx.commit();
  assert.equal(res.deduped, false);
  await assert.rejects(
    tx.execute({ action: "create" }),
    (e: unknown) => isCode(e, "INVALID_ARGUMENT"),
  );
});

test("a late response after a timeout is skipped (self-healing)", async (t) => {
  let calls = 0;
  const addr = await scripted(t, (req) => {
    calls += 1;
    const delay = calls === 1 ? 300 : 0;
    return [[respond(req["id"], "0.2.0"), delay]];
  });
  const c = await dial(t, addr);
  await assert.rejects(
    withDeadline(50, (signal) => c.initialize({ signal })),
    (e: unknown) => isCode(e, "TIMEOUT"),
  );
  // The late frame for request 1 arrives during this call and is skipped
  // (id 1 < id 2); the real answer for id 2 lands.
  await c.initialize();
});
