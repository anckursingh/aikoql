// Shared scripted-server helpers for the wire and pool tests — the ports
// of the Rust suite's scripted/respond/tool_result (client.rs tests) and
// the Go suite's fakeServer. Each scripted server accepts one client
// connection and, per incoming line, hands the parsed request to `script`
// and writes back each returned frame after its delay (an empty frame list
// means a hung server, exactly like the Rust helper). Frame sequences are
// serialized on a promise chain — the stand-in for the Rust helper's single
// spawned task — and the server stops listening at test end.

import net from "node:net";
import type { TestContext } from "node:test";
import { Client } from "../src/index.ts";

export type Frame = [string, number]; // [json line, delay ms]

export function scripted(
  t: TestContext,
  script: (req: Record<string, unknown>) => Frame[],
): Promise<string> {
  let chain: Promise<void> = Promise.resolve();
  const server = net.createServer((socket) => {
    let buf = "";
    socket.on("data", (chunk: Buffer) => {
      buf += chunk.toString("utf8");
      let idx: number;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        const req = JSON.parse(line) as Record<string, unknown>;
        chain = chain.then(async () => {
          for (const [frame, delay] of script(req)) {
            if (delay > 0) await new Promise((r) => setTimeout(r, delay));
            socket.write(frame + "\n");
          }
        });
      }
    });
  });
  t.after(() => server.close());
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      resolve(`127.0.0.1:${(server.address() as net.AddressInfo).port}`);
    });
  });
}

export function respond(id: unknown, version: string): string {
  return JSON.stringify({ id, result: { serverInfo: { version } } });
}

export function toolResult(id: unknown, data: unknown): string {
  return JSON.stringify({
    id,
    result: { content: [{ text: JSON.stringify({ ok: true, data }) }], isError: false },
  });
}

/** Dials and arranges the close for test teardown. */
export async function dial(t: TestContext, addr: string): Promise<Client> {
  const c = await Client.dial(addr);
  t.after(() => {
    void c.close().catch(() => {});
  });
  return c;
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
