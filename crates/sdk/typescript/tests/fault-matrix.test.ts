// D-16 fault matrix (line wire): the §18 fault proxy sits between the SDK
// and a real server and mangles the newline-delimited JSON-RPC wire (§7 of
// the testing plan: every SDK passes the same matrix against the same
// misbehaving server). Frame accounting: client line #1 = initialize, #2 =
// the victim call, #3 = the follow-up (no HELLO/AUTH frames on the MCP
// wire). Mirrors crates/sdk/rust/tests/fault.rs on the line wire.
//
// The contracts GREEN must make hold (the proxy --wire line arm + the
// client's MAX_FRAME cap and closed latch):
//   drop-request/drop-response → victim TIMEOUT (retryable), follow-up ok
//   delay-response → tight deadline TIMEOUT; generous deadline ok
//   duplicate-response → both ok (id correlation skips the duplicate)
//   reorder-response → victim TIMEOUT (response held), follow-up ok
//   truncate-frame → victim TIMEOUT (the missing bytes never arrive),
//     follow-up ok (the wire is self-delimiting)
//   corrupt-frame → the line is noise per the frozen §3.3 semantics →
//     victim TIMEOUT, follow-up ok
//   inject-notification → both ok (an id-less frame is never a response)
//   inject-stale-response → both ok (stale id skipped)
//   close/half-close → victim ok, follow-up fails fast (never TIMEOUT),
//     the client latches closed → UNAVAILABLE from then on
//   slow-server → tight deadline TIMEOUT
//   oversized-response → FRAME_TOO_LARGE before buffering past the 1 MiB
//     cap (§19: a malicious server cannot cause unbounded client memory),
//     follow-up UNAVAILABLE (the stream is desynced — latched)

import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { test, type TestContext } from "node:test";
import { Client, McpError, withDeadline } from "../src/index.ts";

const TOKEN = "test-token";
// npm test / node --test run from the package dir → 3 ups = the repo root.
const ROOT = path.resolve(process.cwd(), "..", "..", "..");

function findBin(name: string): string | undefined {
  const p = path.join(ROOT, "target", "debug", name + (process.platform === "win32" ? ".exe" : ""));
  return fs.existsSync(p) ? p : undefined;
}

const MCP_BIN = process.env.AIKOQL_MCP_BIN ?? findBin("aikoql-mcp");
const PROXY_BIN = process.env.AIKOQL_FAULT_PROXY ?? findBin("aikoql-fault-proxy");

function requireBins(t: TestContext): boolean {
  if (!MCP_BIN || !PROXY_BIN) {
    t.skip("AIKOQL_MCP_BIN / aikoql-fault-proxy not built — real-server fault matrix skipped");
    return false;
  }
  return true;
}

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

/** A real server + one fault-proxy instance (one fault mode) behind the
 * proxy address. The port-probe idiom races → 3 spawn attempts. */
async function faultEnv(
  mode: string,
  extra: string[],
): Promise<{ addr: string; stop: () => void }> {
  let last: Error | undefined;
  for (let attempt = 0; attempt < 3; attempt++) {
    const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "aikoql-ts-fault-"));
    const cleanup = async () => {
      await fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
    };
    let srv: ChildProcess | undefined;
    let px: ChildProcess | undefined;
    try {
      const srvPort = await freePort();
      const db = path.join(dir, "db.aikoql"); // does not exist → auto-create (aikoql-v2)
      srv = spawn(MCP_BIN!, ["serve", db, "--listen", `127.0.0.1:${srvPort}`, "--tcp-token", `${TOKEN}::admin`], {
        stdio: ["ignore", "ignore", fs.openSync(path.join(dir, "srv.err"), "w")],
        windowsHide: true,
      });
      await waitUp(srvPort, srv, path.join(dir, "srv.err"), "aikoql-mcp");

      const pxPort = await freePort();
      const errFile = path.join(dir, "proxy.err");
      px = spawn(
        PROXY_BIN!,
        ["--listen", `127.0.0.1:${pxPort}`, "--target", `127.0.0.1:${srvPort}`, "--wire", "line", "--mode", mode, ...extra],
        { stdio: ["ignore", "ignore", fs.openSync(errFile, "w")], windowsHide: true },
      );
      await waitUp(pxPort, px, errFile, "aikoql-fault-proxy");
      const addr = `127.0.0.1:${pxPort}`;
      return {
        addr,
        stop: () => {
          px!.kill();
          srv!.kill();
          void cleanup();
        },
      };
    } catch (e) {
      last = e as Error;
      px?.kill();
      srv?.kill();
      await cleanup();
    }
  }
  throw last;
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

async function dial(t: TestContext, addr: string): Promise<Client> {
  const c = (await Client.dial(addr)).withToken(TOKEN);
  t.after(() => {
    void c.close().catch(() => {});
  });
  await c.initialize();
  return c;
}

/** Runs fn under the deadline and returns the error code ("" = ok). */
async function codeOf(fn: (signal: AbortSignal) => Promise<unknown>): Promise<string> {
  try {
    await withDeadline(5000, fn);
    return "";
  } catch (e) {
    if (e instanceof McpError) return e.code;
    return "raw:" + String(e);
  }
}

const call = (c: Client) => (signal: AbortSignal) => c.callTool("health", {}, { signal });

async function victim(c: Client, ms: number): Promise<string> {
  try {
    await withDeadline(ms, (signal) => c.callTool("health", {}, { signal }));
    return "";
  } catch (e) {
    if (e instanceof McpError) return e.code;
    return "raw:" + String(e);
  }
}

test("fault matrix: drop-request", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("drop-request", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  const code = await victim(c, 200);
  assert.equal(code, "TIMEOUT");
  assert.equal(await codeOf(call(c)), ""); // the follow-up survives
});

test("fault matrix: drop-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("drop-response", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  const code = await victim(c, 200);
  assert.equal(code, "TIMEOUT");
  assert.equal(await codeOf(call(c)), "");
});

test("fault matrix: delay-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("delay-response", ["--from", "2", "--delay-ms", "400"]);
  t.after(stop);
  const c = await dial(t, addr);
  const code = await victim(c, 200);
  assert.equal(code, "TIMEOUT");
  const { addr: addr2, stop: stop2 } = await faultEnv("delay-response", ["--from", "2", "--delay-ms", "400"]);
  t.after(stop2);
  const c2 = await dial(t, addr2);
  assert.equal(await victim(c2, 2000), ""); // a generous deadline absorbs the delay
});

test("fault matrix: duplicate-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("duplicate-response", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 2000), "");
  assert.equal(await codeOf(call(c)), ""); // the duplicate is a stale id — skipped
});

test("fault matrix: reorder-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("reorder-response", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 200), "TIMEOUT");
  assert.equal(await codeOf(call(c)), ""); // request #3 releases response #2
});

test("fault matrix: truncate-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("truncate-response", ["--n", "2", "--bytes", "8"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 200), "TIMEOUT");
  assert.equal(await codeOf(call(c)), ""); // the wire is self-delimiting
});

test("fault matrix: corrupt-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("corrupt-response", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 200), "TIMEOUT"); // noise-skip, never a fast error
  assert.equal(await codeOf(call(c)), "");
});

test("fault matrix: inject-notification", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("inject-notification", ["--after", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 2000), "");
  assert.equal(await codeOf(call(c)), ""); // an id-less frame is never a response
});

test("fault matrix: inject-stale-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("inject-stale-response", ["--after", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 2000), "");
  assert.equal(await codeOf(call(c)), ""); // the replayed initialize response is stale
});

test("fault matrix: close-after", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("close-after", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 2000), "");
  const follow = await codeOf(call(c));
  assert.notEqual(follow, "");
  assert.notEqual(follow, "TIMEOUT"); // fails fast, never hangs
  assert.equal(await codeOf(call(c)), "UNAVAILABLE"); // latched
});

test("fault matrix: half-close-after", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("half-close-after", ["--n", "2"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 2000), "");
  const follow = await codeOf(call(c));
  assert.notEqual(follow, "");
  assert.notEqual(follow, "TIMEOUT");
  assert.equal(await codeOf(call(c)), "UNAVAILABLE");
});

test("fault matrix: slow-server", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("slow-server", ["--from", "2", "--bytes", "4", "--delay-ms", "25"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 200), "TIMEOUT");
});

test("fault matrix: oversized-response", async (t) => {
  if (!requireBins(t)) return;
  const { addr, stop } = await faultEnv("oversized-response", ["--n", "2", "--claim", "67108864"]);
  t.after(stop);
  const c = await dial(t, addr);
  assert.equal(await victim(c, 5000), "FRAME_TOO_LARGE"); // the 1 MiB cap, not 64 MiB
  assert.equal(await codeOf(call(c)), "UNAVAILABLE"); // the stream is desynced — latched
});
