// AG-60: the Vercel AI SDK adapter — aikoql memory tools for `generateText`
// and friends. The gate asserts the ai v7 tool contract ({description,
// inputSchema, execute} — what the SDK dispatches during a tool call) and
// round-trips the tools against a LIVE server, the same shape the ai SDK
// drives: execute(input, {toolCallId, messages}).

import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { test, type TestContext } from "node:test";

import { createAikoqlTools } from "../src/vercel-ai.ts";
import { Client } from "../src/index.ts";

const TOKEN = "test-token";
// npm test / node --test run from the package dir → 3 ups = the repo root.
const ROOT = path.resolve(process.cwd(), "..", "..", "..");
const MCP_BIN =
  process.env.AIKOQL_MCP_BIN ??
  (() => {
    const p = path.join(
      ROOT,
      "target",
      "debug",
      "aikoql-mcp" + (process.platform === "win32" ? ".exe" : ""),
    );
    return fs.existsSync(p) ? p : undefined;
  })();

function probe(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const s = net.connect(port, "127.0.0.1");
    s.once("connect", () => {
      s.destroy();
      resolve(true);
    });
    s.once("error", () => resolve(false));
  });
}

async function freePort(): Promise<number> {
  const probeSrv = net.createServer();
  await new Promise<void>((resolve, reject) => {
    probeSrv.once("error", reject);
    probeSrv.listen(0, "127.0.0.1", resolve);
  });
  const port = (probeSrv.address() as net.AddressInfo).port;
  await new Promise<void>((resolve) => probeSrv.close(() => resolve()));
  return port;
}

async function waitUp(port: number, child: ChildProcess, errFile: string, what: string): Promise<void> {
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null) {
      const err = await fs.promises.readFile(errFile, "utf8").catch(() => "");
      throw new Error(`${what} exited early (${child.exitCode}):\n${err}`);
    }
    if (await probe(port)) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${what} never listened on 127.0.0.1:${port}`);
}

/** A live aikoql server + a dialed client (one temp dir per attempt, so a
 * port race retries cleanly). */
async function liveServer(t: TestContext): Promise<Client> {
  let last: Error | undefined;
  for (let attempt = 0; attempt < 3; attempt++) {
    const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "aikoql-ts-ai-"));
    const errFile = path.join(dir, "srv.err");
    let srv: ChildProcess | undefined;
    try {
      const port = await freePort();
      srv = spawn(
        MCP_BIN!,
        ["serve", path.join(dir, "db.aikoql"), "--listen", `127.0.0.1:${port}`, "--tcp-token", `${TOKEN}::admin`],
        { stdio: ["ignore", "ignore", fs.openSync(errFile, "w")], windowsHide: true },
      );
      await waitUp(port, srv, errFile, "aikoql-mcp");
      const client = (await Client.dial(`127.0.0.1:${port}`)).withToken(TOKEN);
      await client.initialize();
      t.after(() => {
        srv!.kill();
        void client.close().catch(() => {});
        void fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
      });
      return client;
    } catch (e) {
      last = e as Error;
      srv?.kill();
      await fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
    }
  }
  throw last;
}

/** The ai v7 dispatch shape the test drives (ToolSet types execute's input
 * as never, so the round-trip goes through this alias). */
type AiTool = {
  description?: unknown;
  inputSchema?: unknown;
  execute?: (input: Record<string, unknown>, options: unknown) => Promise<unknown>;
};

test("vercel ai: tools meet the ai contract and round-trip a live server", async (t) => {
  if (!MCP_BIN) {
    t.skip("AIKOQL_MCP_BIN / aikoql-mcp not built — live-server gate skipped");
    return;
  }
  const client = await liveServer(t);
  const tools = createAikoqlTools(client, "vercel-ai-ag60") as unknown as Record<string, AiTool>;

  // the ai v7 tool contract (what generateText dispatches)
  for (const name of ["recall", "remember", "forget"]) {
    const tk = tools[name];
    assert.ok(tk, `${name} tool present`);
    assert.equal(typeof tk.description, "string", `${name}.description`);
    assert.equal(typeof tk.inputSchema, "object", `${name}.inputSchema`);
    assert.equal(typeof tk.execute, "function", `${name}.execute`);
  }

  // round-trip through execute() — the call shape the ai SDK uses
  const remembered = (await tools.remember!.execute!(
    { type_name: "note", properties: { text: "gate note from the vercel test" } },
    { toolCallId: "c1", messages: [] },
  )) as { koid: string };
  assert.ok(remembered.koid, `remember returned a koid: ${JSON.stringify(remembered)}`);

  const hits = (await tools.recall!.execute!(
    { query: "gate note" },
    { toolCallId: "c2", messages: [] },
  )) as unknown[];
  assert.ok(hits.length >= 1, `recall found the stored note: ${JSON.stringify(hits)}`);

  await tools.forget!.execute!(
    { koid: remembered.koid },
    { toolCallId: "c3", messages: [] },
  );

  const after = (await tools.recall!.execute!(
    { query: "gate note" },
    { toolCallId: "c4", messages: [] },
  )) as unknown[];
  assert.equal(after.length, 0, `forget tombstoned the note: ${JSON.stringify(after)}`);
});
