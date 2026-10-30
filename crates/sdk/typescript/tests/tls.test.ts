// The SDK over TLS against the real server started with --tls-cert /
// --tls-key (the MCP crate's test certificates, SAN IP:127.0.0.1):
// Client.dial(addr, { tls: { ca } }) initializes and round-trips a write; a
// server outside the trust anchors is refused at the dial, before the token
// is sent. Skipped without AIKOQL_MCP_BIN.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { fileURLToPath } from "node:url";
import { Client } from "../src/index.ts";
import { sleep } from "./helpers.ts";

const FX = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../services/api/mcp/tests/fixtures/tls",
);
const ca = (name: string) => fs.readFileSync(path.join(FX, name));
const skip =
  process.env.AIKOQL_MCP_BIN === undefined ? "AIKOQL_MCP_BIN not set — TLS real-server test skipped" : false;

async function startTlsServer(t: TestContext): Promise<string> {
  const probe = net.createServer();
  await new Promise<void>((resolve) => probe.listen(0, "127.0.0.1", resolve));
  const port = (probe.address() as net.AddressInfo).port;
  await new Promise<void>((resolve) => probe.close(() => resolve()));
  const addr = `127.0.0.1:${port}`;
  const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "ts-tls-"));
  const child: ChildProcess = spawn(
    process.env.AIKOQL_MCP_BIN!,
    [
      "serve", "--listen", addr, "--tcp-token", "ts-tls::admin",
      "--tls-cert", path.join(FX, "server.pem"), "--tls-key", path.join(FX, "server.key"),
      path.join(dir, "kb"),
    ],
    { stdio: ["ignore", "ignore", "ignore"], windowsHide: true },
  );
  t.after(async () => {
    child.kill();
    if (child.exitCode === null) await new Promise<void>((resolve) => child.once("exit", () => resolve()));
    await fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
  });
  const deadline = Date.now() + 10_000;
  for (;;) {
    const up = await new Promise<boolean>((resolve) => {
      const s = net.connect(port, "127.0.0.1");
      s.once("connect", () => { s.destroy(); resolve(true); });
      s.once("error", () => resolve(false));
    });
    if (up) return addr;
    if (Date.now() >= deadline) throw new Error(`TLS server never came up on ${addr}`);
    await sleep(100);
  }
}

test("MCP over TLS", { skip }, async (t) => {
  const addr = await startTlsServer(t);
  const c = (await Client.dial(addr, { tls: { ca: ca("ca.pem") } })).withToken("ts-tls");
  t.after(() => c.close());
  await c.initialize();
  const put = (await c.callTool("remember", { subject: "admin", type_name: "note", properties: { n: 7 } })) as { koid: string };
  assert.ok(put.koid, "remember over TLS returns a koid");
  const got = (await c.callTool("get", { subject: "admin", koid: put.koid })) as { properties: { n: number } };
  assert.equal(got.properties.n, 7);
});

test("an untrusted server is refused at the dial", { skip }, async (t) => {
  const addr = await startTlsServer(t);
  await assert.rejects(Client.dial(addr, { tls: { ca: ca("other-ca.pem") } }));
});
