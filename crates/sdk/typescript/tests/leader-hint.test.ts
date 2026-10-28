// MRFC-0080 R5b — the client follows a NOT_LEADER leader hint: a fenced
// server refuses a write with NOT_LEADER and the newer leader's client
// address; the call is retried once against that leader over a fresh,
// initialized connection, and the client stays there.

import assert from "node:assert/strict";
import { test } from "node:test";
import { McpError, MIN_SERVER_VERSION } from "../src/index.ts";
import { dial, respond, scripted, toolResult } from "./helpers.ts";

function notLeader(id: unknown, leader?: string): string {
  const error: Record<string, unknown> = { code: "NOT_LEADER", message: "not leader", retryable: true };
  if (leader !== undefined) error.leader = leader;
  return JSON.stringify({
    id,
    result: { content: [{ text: JSON.stringify({ ok: false, error }) }], isError: true },
  });
}

test("NOT_LEADER follows the leader hint", async (t) => {
  const saw: string[] = [];
  const leader = await scripted(t, (req) => {
    saw.push(req["method"] as string);
    return req["method"] === "initialize"
      ? [[respond(req["id"], MIN_SERVER_VERSION), 0]]
      : [[toolResult(req["id"], { koid: "ko-1" }), 0]];
  });
  const fenced = await scripted(t, (req) =>
    req["method"] === "initialize"
      ? [[respond(req["id"], MIN_SERVER_VERSION), 0]]
      : [[notLeader(req["id"], leader), 0]],
  );
  const c = (await dial(t, fenced)).withToken("t");
  await c.initialize();
  assert.deepEqual(await c.callTool("remember", { type_name: "note" }), { koid: "ko-1" });
  await c.callTool("remember", { type_name: "note" });
  assert.deepEqual(saw, ["initialize", "tools/call", "tools/call"]);
});

test("NOT_LEADER without a hint is thrown", async (t) => {
  const addr = await scripted(t, (req) => [[notLeader(req["id"]), 0]]);
  const c = await dial(t, addr);
  await assert.rejects(c.callTool("remember", {}), (e: unknown) => {
    assert.ok(e instanceof McpError);
    assert.equal(e.code, "NOT_LEADER");
    assert.equal(e.retryable, true);
    assert.equal(e.leader, undefined);
    return true;
  });
});
