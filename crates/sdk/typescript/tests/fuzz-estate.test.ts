// D-16 §10: the TS fuzz estate — fast-check property tests (L1/L3) and
// the §17 command model (L2: DISCONNECTED→CONNECTED→INITIALIZED→
// TRANSACTION→STREAMING→CLOSED), the TypeScript arm of the five-language
// estate. The §17 property rules the whole file: illegal transition
// sequences must error deterministically — the exact frozen code — and
// never hang, panic, or let a success escape an illegal step. Everything
// runs in-process against a scripted loopback server; the socket layer
// itself is covered by the wire tests.

import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";
import {
  assert as fcAssert,
  asyncModelRun,
  asyncProperty,
  boolean,
  commands,
  constant,
  integer,
  jsonValue,
  oneof,
  option,
  property,
  record,
  string,
  uint8Array,
} from "fast-check";

import { Client, McpError, withDeadline } from "../src/index.ts";
import { MIN_SERVER_VERSION, classifyID, parseVersion, rpcError, versionLess } from "../src/client.ts";
import { MAX_FRAME } from "../src/tcp.ts";
import type { Tx } from "../src/tx.ts";
import { respond, toolResult } from "./helpers.ts";

/** A scripted loopback server for one test body: per incoming line, hands
 * the parsed request to `script` and writes each returned frame after a
 * newline (an empty list means a hung server, exactly like the Rust
 * helper). */
function withResponder<T>(
  script: (req: Record<string, unknown>) => string[],
  body: (addr: string) => Promise<T>,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      let buf = "";
      socket.on("data", (chunk: Buffer) => {
        buf += chunk.toString("utf8");
        let idx: number;
        while ((idx = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, idx);
          buf = buf.slice(idx + 1);
          const req = JSON.parse(line) as Record<string, unknown>;
          for (const f of script(req)) socket.write(f + "\n");
        }
      });
    });
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = `127.0.0.1:${(server.address() as net.AddressInfo).port}`;
      body(addr).then(resolve, reject).finally(() => server.close());
    });
  });
}

function isCode(code: string): (e: unknown) => boolean {
  return (e) => e instanceof McpError && e.code === code;
}

function isMcp(e: unknown, code: string, retryable: boolean): boolean {
  return e instanceof McpError && e.code === code && e.retryable === retryable;
}

// -- L1/L3 property tests ---------------------------------------------

test("the version-parse property", async () => {
  await fcAssert(
    property(string(), (v) => {
      const parts = parseVersion(v);
      assert.equal(parts.length, v.split(".").length);
      v.split(".").forEach((seg, i) => {
        // The frozen mirror restated: int(seg) semantics — only an
        // optionally-signed decimal integer segment is numeric (Number
        // alone would read "" as 0 and "0x10" as 16 where int() refuses).
        const s = seg.trim();
        const want = /^[+-]?\d+$/.test(s) ? Number(s) : -1;
        assert.equal(parts[i], want);
      });
      assert.equal(versionLess(parts, parts), false); // irreflexive
    }),
  );
});

test("the error-mapping property", async () => {
  await fcAssert(
    property(
      record({
        code: option(oneof(string(), integer(), boolean())),
        message: option(string()),
      }),
      (wire) => {
        const e = rpcError(wire as { code?: unknown; message?: string });
        // The frozen mapping restated: absent codes become INTERNAL, and
        // the message default is the empty string.
        const c = wire.code === undefined || wire.code === null ? "" : String(wire.code);
        const code = c === "" ? "INTERNAL" : c;
        assert.equal(e.code, code);
        // The TS-frozen mirror: Error.message carries the [code]
        // decoration itself (the Python SDK keeps .message raw and
        // decorates str()) — the §16 corpus decision notes the divergence.
        assert.equal(e.message, `[${code}] ${wire.message ?? ""}`);
        assert.equal(e.retryable, false);
        assert.equal(e.suggestion, "");
      },
    ),
  );
});

test("the id-correlation property", async () => {
  await fcAssert(
    property(integer(), integer(), (want, got) => {
      // The frozen §3.3 rules restated: smaller ids skip, larger ids are
      // PROTOCOL_ERROR, equal ids match.
      const wantClass = got < want ? "skip" : got > want ? "protocol" : "match";
      assert.equal(classifyID(want, got), wantClass);
    }),
  );
});

test("the frame-cap property", async () => {
  await fcAssert(
    asyncProperty(uint8Array({ maxLength: MAX_FRAME + 4096 }), async (junk) => {
      // The §19 bound on the wire: FRAME_TOO_LARGE exactly when the junk
      // runs past the cap, the client latches, and the next call fails
      // UNAVAILABLE. latin1 keeps the junk byte-exact through the frame
      // string; the cap check fires before any newline split.
      await withResponder(() => [Buffer.from(junk).toString("latin1")], async (addr) => {
        const c = await Client.dial(addr);
        try {
          let outcome = "ok";
          try {
            await withDeadline(250, (signal) => c.callTool("probe", {}, { signal }));
          } catch (e) {
            if (e instanceof McpError) outcome = e.code;
            else throw e;
          }
          if (junk.length > MAX_FRAME) {
            assert.equal(outcome, "FRAME_TOO_LARGE");
            await assert.rejects(c.callTool("probe"), isCode("UNAVAILABLE"));
          } else {
            assert.equal(outcome, "TIMEOUT"); // never a cap error under the cap
          }
        } finally {
          await c.close().catch(() => {});
        }
      });
    }),
    { numRuns: 40 },
  );
});

test("the envelope property", async () => {
  await fcAssert(
    asyncProperty(jsonValue(), async (env) => {
      // ok:true round-trips the data payload.
      await withResponder(
        (req) => [
          JSON.stringify({
            id: req["id"],
            result: {
              content: [{ text: JSON.stringify({ ok: true, data: env }) }],
              isError: false,
            },
          }),
        ],
        async (addr) => {
          const c = await Client.dial(addr);
          try {
            assert.deepEqual(await c.callTool("probe"), env);
          } finally {
            await c.close().catch(() => {});
          }
        },
      );
      // ok:false never escapes as success — the frozen INTERNAL.
      await withResponder(
        (req) => [
          JSON.stringify({
            id: req["id"],
            result: {
              content: [{ text: JSON.stringify({ ok: false, data: env }) }],
              isError: false,
            },
          }),
        ],
        async (addr) => {
          const c = await Client.dial(addr);
          try {
            await assert.rejects(c.callTool("probe"), isCode("INTERNAL"));
          } finally {
            await c.close().catch(() => {});
          }
        },
      );
      // Garbage text is a JSON classification, never a success payload.
      await withResponder(
        (req) => [
          JSON.stringify({
            id: req["id"],
            result: { content: [{ text: "not an envelope" }], isError: false },
          }),
        ],
        async (addr) => {
          const c = await Client.dial(addr);
          try {
            await assert.rejects(c.callTool("probe"), isCode("JSON"));
          } finally {
            await c.close().catch(() => {});
          }
        },
      );
    }),
    { numRuns: 40 },
  );
});

// -- the §17 command model --------------------------------------------

class ProtocolModel {
  state = "DISCONNECTED";
}

interface Wire {
  addr: string;
  client: Client | null;
  tx: Tx | null;
  gen: AsyncGenerator<unknown, void, unknown> | null;
  ctrl: AbortController | null;
  script: (req: Record<string, unknown>) => string[];
  sendRaw: (b: Buffer) => void;
  endSocket: () => void;
  close: () => void;
  trace: string;
}

/** A scripted server for one model run: the script slot is reassigned per
 * command, sendRaw pushes a frame without a request (the notify leg), and
 * endSocket kills the transport in place. */
function modelWire(): Promise<Wire> {
  const wire = {
    addr: "",
    client: null,
    tx: null,
    gen: null,
    ctrl: null,
    script: (_req: Record<string, unknown>): string[] => [],
    sendRaw: (_b: Buffer) => {},
    endSocket: () => {},
    close: () => {},
    trace: "",
  } as Wire;
  return new Promise((resolve) => {
    const server = net.createServer((socket) => {
      wire.sendRaw = (b) => socket.write(b);
      wire.endSocket = () => socket.end();
      let buf = "";
      socket.on("data", (chunk: Buffer) => {
        buf += chunk.toString("utf8");
        let idx: number;
        while ((idx = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, idx);
          buf = buf.slice(idx + 1);
          const req = JSON.parse(line) as Record<string, unknown>;
          for (const f of wire.script(req)) {
            // A dead peer swallows the frame (the real server's EPIPE):
            // after endSocket the socket must refuse writes, not throw.
            if (socket.writableEnded || socket.destroyed) break;
            socket.write(f + "\n");
          }
        }
      });
    });
    wire.close = () => server.close();
    server.listen(0, "127.0.0.1", () => {
      wire.addr = `127.0.0.1:${(server.address() as net.AddressInfo).port}`;
      resolve(wire);
    });
  });
}

class Cmd {
  private name: string;
  private when: (m: ProtocolModel) => boolean;
  private exec: (m: ProtocolModel, w: Wire) => Promise<void>;

  constructor(
    name: string,
    when: (m: ProtocolModel) => boolean,
    exec: (m: ProtocolModel, w: Wire) => Promise<void>,
  ) {
    this.name = name;
    this.when = when;
    this.exec = exec;
  }

  // A false check discards the command for this step (fast-check's
  // precondition) — the frozen states gate every leg.
  check(m: ProtocolModel): boolean {
    return this.when(m);
  }

  run(m: ProtocolModel, w: Wire): Promise<void> {
    w.trace = this.name; // names the stalled leg in the §17 watchdog
    return this.exec(m, w);
  }

  toString(): string {
    return this.name;
  }
}

const CONNECTED = new Set(["CONNECTED", "INITIALIZED", "TRANSACTION", "STREAMING"]);
const ALIVE = new Set(["CONNECTED", "INITIALIZED", "TRANSACTION"]);

const COMMANDS: Cmd[] = [
  new Cmd(
    "connect",
    (m) => m.state === "DISCONNECTED" || m.state === "CLOSED",
    async (m, w) => {
      // A fresh dial is a fresh transport; a latched client is never
      // reconnected in place (the SDK has no recovery path).
      w.client = await Client.dial(w.addr);
      m.state = "CONNECTED";
    },
  ),
  new Cmd("close", (m) => CONNECTED.has(m.state), async (m, w) => {
    await w.client!.close();
    m.state = "CLOSED";
  }),
  new Cmd("callUnavailable", (m) => m.state === "CLOSED", async (_m, w) => {
    // The §17 illegal transition: any call from a closed client refuses
    // with UNAVAILABLE before touching the socket.
    await assert.rejects(w.client!.callTool("probe"), isCode("UNAVAILABLE"));
  }),
  new Cmd("initializeOk", (m) => m.state === "CONNECTED", async (m, w) => {
    w.script = (req) => [respond(req["id"], MIN_SERVER_VERSION)];
    await w.client!.initialize();
    m.state = "INITIALIZED";
  }),
  new Cmd("initializeOld", (m) => m.state === "CONNECTED", async (_m, w) => {
    w.script = (req) => [respond(req["id"], "0.0.1")];
    await assert.rejects(w.client!.initialize(), isCode("VERSION_MISMATCH"));
    // still CONNECTED — the handshake failed, the transport is alive
  }),
  new Cmd("initializeSilent", (m) => m.state === "CONNECTED", async (_m, w) => {
    w.script = () => [];
    // The signal must be threaded — an unthreaded abort never reaches
    // readLine and the call hangs forever (the D-13 trap).
    await assert.rejects(
      withDeadline(80, (signal) => w.client!.initialize({ signal })),
      (e) => isMcp(e, "TIMEOUT", true),
    );
    // still CONNECTED
  }),
  new Cmd("begin", (m) => m.state === "INITIALIZED", async (m, w) => {
    w.script = (req) => [toolResult(req["id"], { txn_id: "t" })];
    w.tx = await w.client!.begin("t");
    m.state = "TRANSACTION";
  }),
  new Cmd("stage", (m) => m.state === "TRANSACTION", async (_m, w) => {
    w.script = (req) => [toolResult(req["id"], {})];
    await w.tx!.execute({ action: "create", type_name: "thing" });
    // still TRANSACTION
  }),
  new Cmd("commit", (m) => m.state === "TRANSACTION", async (m, w) => {
    w.script = (req) => [toolResult(req["id"], {})];
    await w.tx!.commit();
    m.state = "INITIALIZED";
  }),
  new Cmd("rollback", (m) => m.state === "TRANSACTION", async (m, w) => {
    w.script = (req) => [toolResult(req["id"], {})];
    await w.tx!.rollback();
    m.state = "INITIALIZED";
  }),
  new Cmd("commitReuse", (m) => m.state === "TRANSACTION", async (m, w) => {
    // The phantom-commit guard: a closed handle refuses.
    w.script = (req) => [toolResult(req["id"], {})];
    await w.tx!.commit();
    m.state = "INITIALIZED";
    await assert.rejects(
      w.tx!.execute({ action: "create", type_name: "thing" }),
      isCode("INVALID_ARGUMENT"),
    );
  }),
  new Cmd("streamOpen", (m) => m.state === "INITIALIZED", async (m, w) => {
    w.script = (req) => [
      JSON.stringify({
        id: req["id"],
        result: { stream_id: "s", total_chunks: 2, results: [] },
      }),
    ];
    w.ctrl = new AbortController();
    w.gen = w.client!.queryStream("MATCH x", "", { signal: w.ctrl.signal });
    const head = (await w.gen.next()).value as { stream_id?: string };
    assert.equal(head.stream_id, "s");
    m.state = "STREAMING";
  }),
  new Cmd("streamNextDone", (m) => m.state === "STREAMING", async (m, w) => {
    // The notify is a push — it rides sendRaw, not a request script.
    w.sendRaw(
      Buffer.from(
        JSON.stringify({
          method: "notifications/notify",
          params: { stream_id: "s", done: true },
        }) + "\n",
      ),
    );
    const chunk = (await w.gen!.next()).value as { done?: boolean };
    assert.equal(chunk.done, true);
    m.state = "INITIALIZED";
  }),
  new Cmd("streamNextTimeout", (m) => m.state === "STREAMING", async (m, w) => {
    // A truncated stream: aborting the pending read surfaces as the frozen
    // retryable TIMEOUT (the abort IS the stream-cancel).
    const p = w.gen!.next();
    w.ctrl!.abort();
    await assert.rejects(p, (e) => isMcp(e, "TIMEOUT", true));
    m.state = "INITIALIZED"; // the generator died, the client lives
  }),
  new Cmd("streamAbandon", (m) => m.state === "STREAMING", async (m, w) => {
    // The abort IS the stream-cancel: the generator is mid-read after the
    // head, and gen.return() would queue behind the pending await forever.
    w.ctrl!.abort();
    await w.gen!.return().catch(() => {});
    m.state = "INITIALIZED";
  }),
  new Cmd("protocolStaleId", (m) => m.state === "INITIALIZED", async (_m, w) => {
    w.script = (req) => [
      JSON.stringify({ id: ((req["id"] as number) ?? 0) - 1, result: {} }),
      toolResult(req["id"], {}),
    ];
    await w.client!.callTool("probe"); // the stale id is skipped, the real one lands
  }),
  new Cmd("protocolImpossibleId", (m) => m.state === "INITIALIZED", async (_m, w) => {
    w.script = (req) => [
      JSON.stringify({ id: ((req["id"] as number) ?? 0) + 1, result: {} }),
    ];
    await assert.rejects(w.client!.callTool("probe"), isCode("PROTOCOL_ERROR"));
  }),
  new Cmd("protocolNonNumericId", (m) => m.state === "INITIALIZED", async (_m, w) => {
    // The TS-frozen divergence from the Python SDK: a non-numeric id is
    // skipped (never PROTOCOL_ERROR) — the call then times out.
    w.script = (req) => [JSON.stringify({ id: "nope", result: {} })];
    await assert.rejects(
      withDeadline(80, (signal) => w.client!.callTool("probe", {}, { signal })),
      (e) => isMcp(e, "TIMEOUT", true),
    );
  }),
  new Cmd("protocolMalformed", (m) => m.state === "INITIALIZED", async (_m, w) => {
    w.script = (req) => ["garbage", toolResult(req["id"], {})];
    await w.client!.callTool("probe"); // garbage is skipped, never misread
  }),
  new Cmd(
    "rpcTimeout",
    (m) => m.state === "CONNECTED" || m.state === "INITIALIZED",
    async (_m, w) => {
      w.script = () => [];
      await assert.rejects(
        withDeadline(80, (signal) => w.client!.callTool("probe", {}, { signal })),
        (e) => isMcp(e, "TIMEOUT", true),
      );
      // TIMEOUT never latches (§19) — the state is unchanged
    },
  ),
  new Cmd("transportDies", (m) => ALIVE.has(m.state), async (m, w) => {
    w.endSocket();
    await assert.rejects(w.client!.callTool("probe"), isCode("IO"));
    await assert.rejects(w.client!.callTool("probe"), isCode("UNAVAILABLE"));
    m.state = "CLOSED";
  }),
  new Cmd("oversized", (m) => ALIVE.has(m.state), async (m, w) => {
    // Served as the response — the cap check fires before any newline
    // split, so one over-cap write is FRAME_TOO_LARGE and a latch.
    w.script = () => ["x".repeat(MAX_FRAME + 1)];
    await assert.rejects(w.client!.callTool("probe"), isCode("FRAME_TOO_LARGE"));
    await assert.rejects(w.client!.callTool("probe"), isCode("UNAVAILABLE"));
    m.state = "CLOSED";
  }),
];

test("the §17 command model", async () => {
  await fcAssert(
    asyncProperty(commands(COMMANDS.map((c) => constant(c)), { maxCommands: 25 }), async (cmds) => {
      const w = await modelWire();
      try {
        // The §17 "never hang" assertion: a stalled run is a property
        // failure (fc reports the seed), not a silent CI hang.
        await Promise.race([
          asyncModelRun(() => ({ model: new ProtocolModel(), real: w }), cmds),
          new Promise<never>((_, reject) =>
            setTimeout(() => reject(new Error(`§17 hang: the model run stalled in ${w.trace}`)), 15000),
          ),
        ]);
      } finally {
        // Fire-and-forget: under a hang, awaiting cleanup would hang too.
        if (w.ctrl) w.ctrl.abort(); // a pending stream read must be aborted first
        void w.gen?.return().catch(() => {});
        void w.client?.close().catch(() => {});
        w.close();
      }
    }),
    { numRuns: 30 },
  );
});
