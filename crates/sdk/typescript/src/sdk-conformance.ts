// Command sdk-conformance is the TypeScript adapter for the shared
// conformance runner (D-13, §7/§23). It executes the language-neutral
// vectors from tests/sdk-conformance/ (the §23 canonical workload + the §7
// category dirs) and protocol/test-vectors/ against a real aikoql-mcp
// server through this SDK, then checks every assert/assert_any/expect_error.
// The expected results are the vectors themselves — every SDK produces the
// same transcript. Mirrors the Rust adapter (crates/sdk/rust/src/bin/
// sdk-conformance.rs) arm for arm.
//
// Run through scripts/sdk-conformance.sh, or directly:
//
//   node src/sdk-conformance.ts --bin <aikoql-mcp> \
//       --vectors <dir> --protocol <dir> --token <token>

import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { Client, McpError, Tx, type StagedOp } from "./index.ts";

interface VectorFile {
  name: string;
  operations: unknown[];
}

type Var = { kind: "koid"; koid: string } | { kind: "tx"; tx: Tx };

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Numeric-aware equality: 3 and 3.0 compare equal, like Python's == and
 * unlike serde_json's Number PartialEq (Go never meets mixed int/float —
 * everything decodes to float64 — but Python does, and the vectors were
 * frozen against both). JS numbers unify int/float already. */
function jsonEq(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((v, i) => jsonEq(v, b[i]));
  }
  if (
    a !== null &&
    b !== null &&
    typeof a === "object" &&
    typeof b === "object" &&
    !Array.isArray(a) &&
    !Array.isArray(b)
  ) {
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    return (
      ka.length === kb.length &&
      ka.every(
        (k) =>
          k in b && jsonEq((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]),
      )
    );
  }
  return a === b;
}

function dotGet(obj: unknown, path: string): unknown {
  let cur = obj;
  for (const part of path.split(".")) {
    if (cur === null || typeof cur !== "object") {
      throw new Error(`cannot descend into ${JSON.stringify(cur)} at ${JSON.stringify(part)}`);
    }
    if (Array.isArray(cur)) {
      const idx = Number(part);
      if (!Number.isInteger(idx)) throw new Error(`no index ${JSON.stringify(part)}`);
      cur = cur[idx];
    } else {
      cur = (cur as Record<string, unknown>)[part];
      if (cur === undefined) throw new Error(`no key ${JSON.stringify(part)}`);
    }
  }
  return cur;
}

/** Wire surfaces → SDK-012 codes: the server's -32001 token rejection is
 * AUTHENTICATION_FAILED to the caller. */
function mapCode(code: string): string {
  return code === "-32001" ? "AUTHENTICATION_FAILED" : code;
}

function props(v: unknown): Record<string, unknown> | undefined {
  return v !== null && typeof v === "object" && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : undefined;
}

function or(a: string, b: string): string {
  return a === "" ? b : a;
}

class Runner {
  addr: string;
  token: string;
  client: Client | null = null;
  vars = new Map<string, Var>();
  last = "";

  constructor(addr: string, token: string) {
    this.addr = addr;
    this.token = token;
  }

  private current(): Client {
    if (this.client === null) throw new McpError("INTERNAL", "vector must connect first");
    return this.client;
  }

  /** ref resolves "$name" against the var map; anything else passes
   * through. */
  private refKoid(v: unknown): string {
    const s = typeof v === "string" ? v : "";
    if (s.startsWith("$")) {
      const k = this.vars.get(s.slice(1));
      if (k?.kind === "koid") return k.koid;
    }
    return s;
  }

  private txVar(op: Record<string, unknown>): Tx {
    const name = (typeof op["txn"] === "string" ? op["txn"] : "").replace(/^\$/, "");
    const v = this.vars.get(name);
    if (v?.kind === "tx") return v.tx;
    throw new McpError("INTERNAL", `var ${JSON.stringify(name)} is not an open txn`);
  }

  async connect(token: string): Promise<void> {
    const c = await Client.dial(this.addr);
    c.withToken(token);
    try {
      await c.initialize();
    } catch (e) {
      await c.close().catch(() => {});
      throw e;
    }
    this.client = c;
  }

  async runOp(op: Record<string, unknown>): Promise<unknown> {
    switch (op["op"]) {
      case "connect": {
        const tok = typeof op["token"] === "string" ? op["token"] : this.token;
        await this.connect(tok);
        return {};
      }
      case "close": {
        if (this.client === null) throw new McpError("INTERNAL", "no client to close");
        await this.client.close();
        // The closed client stays current: a call on it is UNAVAILABLE
        // (§3.3), which is what the cancellation vector expects.
        return {};
      }
      case "health":
        return this.current().health();
      case "metrics":
        return this.current().metrics();
      case "remember": {
        const rem = await this.current().remember({
          type_name: (op["type"] as string) ?? "",
          ...(props(op["properties"]) !== undefined
            ? { properties: props(op["properties"]) }
            : {}),
        });
        this.last = rem.koid;
        return rem;
      }
      case "update": {
        const koid = or(this.refKoid(op["koid"]), this.last);
        const rem = await this.current().remember({
          type_name: (op["type"] as string) ?? "",
          koid,
          ...(props(op["properties"]) !== undefined
            ? { properties: props(op["properties"]) }
            : {}),
        });
        this.last = rem.koid;
        return rem;
      }
      case "get": {
        const koid = or(this.refKoid(op["koid"]), this.last);
        return this.current().get(koid, "");
      }
      case "delete": {
        const koid = or(this.refKoid(op["koid"]), this.last);
        const m = (await this.current().forget(koid, "tombstone", "")) as {
          koid?: string;
        };
        if (typeof m.koid === "string") this.last = m.koid;
        return m;
      }
      case "query": {
        if (op["stream"] === true) {
          const stream = this.current().queryStream((op["query"] as string) ?? "", "");
          const chunks: unknown[] = [];
          for await (const chunk of stream) chunks.push(chunk);
          return { chunks };
        }
        return this.current().aikoql((op["query"] as string) ?? "", "");
      }
      case "relate": {
        const m = (await this.current().relate(
          this.refKoid(op["from"]),
          this.refKoid(op["to"]),
          (op["rel_type"] as string) ?? "",
          "",
        )) as { koid?: string };
        if (typeof m.koid === "string") this.last = m.koid;
        return m;
      }
      case "traverse": {
        const depth = typeof op["depth"] === "number" ? op["depth"] : 1;
        return this.current().traverse(
          this.refKoid(op["koid"]),
          (op["rel_type"] as string) ?? "",
          "",
          depth,
        );
      }
      case "find_similar": {
        const p = {
          ...(typeof op["text"] === "string" ? { text: op["text"] } : {}),
          ...(typeof op["wait_for_freshness_ms"] === "number"
            ? { wait_for_freshness_ms: op["wait_for_freshness_ms"] }
            : {}),
        };
        const hits = await this.current().findSimilar(p);
        return { results: hits };
      }
      case "begin": {
        const tx = await this.current().begin();
        const id = tx.id();
        this.vars.set((op["as"] as string) ?? "", { kind: "tx", tx });
        return { txn_id: id };
      }
      case "execute": {
        const staged: StagedOp = {
          action: (op["action"] as string) ?? "",
          ...((op["type"] as string) !== "" && op["type"] !== undefined
            ? { type_name: op["type"] as string }
            : {}),
          ...(props(op["properties"]) !== undefined
            ? { properties: props(op["properties"]) }
            : {}),
        };
        await this.txVar(op).execute(staged);
        return {};
      }
      case "commit":
        return this.txVar(op).commit();
      case "rollback":
        await this.txVar(op).rollback();
        return { rolled_back: true };
      case "explain": {
        const koid = this.refKoid(op["koid"]);
        return this.current().explain(koid, "");
      }
      case "trace": {
        const koid = this.refKoid(op["koid"]);
        return this.current().trace(koid, "");
      }
      case "discover_schema":
        return this.current().discoverSchema();
      default:
        throw new McpError("INTERNAL", `op ${JSON.stringify(op["op"])} has no adapter arm`);
    }
  }

  private capture(op: Record<string, unknown>, result: unknown): void {
    const asName = typeof op["as"] === "string" ? op["as"] : "";
    if (asName === "") return;
    const r = result as { koid?: string; results?: Array<{ koid?: string }> };
    if (typeof r.koid === "string") {
      this.vars.set(asName, { kind: "koid", koid: r.koid });
      return;
    }
    if (typeof r.results?.[0]?.koid === "string") {
      this.vars.set(asName, { kind: "koid", koid: r.results[0]!.koid! });
    }
  }

  /** "$name" in an expected value resolves against the var map. */
  private deref(want: unknown): unknown {
    if (typeof want === "string" && want.startsWith("$")) {
      const k = this.vars.get(want.slice(1));
      if (k?.kind === "koid") return k.koid;
    }
    return want;
  }

  private check(op: Record<string, unknown>, result: unknown): void {
    const asserts = props(op["assert"]);
    if (asserts !== undefined) {
      for (const [pathName, want] of Object.entries(asserts)) {
        const got = dotGet(result, pathName);
        const wantD = this.deref(want);
        if (!jsonEq(got, wantD)) {
          throw new Error(`assert ${pathName}: expected ${JSON.stringify(wantD)}, got ${JSON.stringify(got)}`);
        }
      }
    }
    const aa = props(op["assert_any"]);
    if (aa !== undefined) {
      const pathName = (aa["path"] as string) ?? "";
      const items = dotGet(result, pathName);
      if (!Array.isArray(items)) {
        throw new Error(`assert_any ${pathName}: not a list`);
      }
      const m = aa["match"];
      if (m === undefined) throw new Error("assert_any: no match");
      let found = false;
      if (props(m) !== undefined) {
        const wantMap = props(m)!;
        for (const e of items) {
          const em = props(e);
          if (em === undefined) continue;
          let all = true;
          for (const [p, v] of Object.entries(wantMap)) {
            try {
              if (!jsonEq(dotGet(em, p), v)) {
                all = false;
                break;
              }
            } catch {
              all = false;
              break;
            }
          }
          if (all) {
            found = true;
            break;
          }
        }
      } else {
        found = items.some((e) => jsonEq(e, m));
      }
      if (!found) {
        throw new Error(`assert_any ${pathName}: no element matches ${JSON.stringify(m)}`);
      }
    }
  }

  async runVector(ops: unknown[]): Promise<void> {
    await this.connect(this.token);
    this.vars.clear();
    this.last = "";
    for (const [i, opv] of ops.entries()) {
      const op = props(opv);
      if (op === undefined) throw new Error(`op ${i}: not an object`);
      const expect = typeof op["expect_error"] === "string" ? op["expect_error"] : "";
      try {
        const result = await this.runOp(op);
        if (expect !== "") {
          throw new Error(`op ${i} ${JSON.stringify(op["op"])}: expected error ${expect}, none raised`);
        }
        this.capture(op, result);
        try {
          this.check(op, result);
        } catch (e) {
          throw new Error(`op ${i} ${JSON.stringify(op["op"])}: ${(e as Error).message}`);
        }
      } catch (e) {
        const code = e instanceof McpError ? mapCode(e.code) : (e as Error).message;
        if (expect === code) continue;
        if (expect !== "") {
          throw new Error(`op ${i} ${JSON.stringify(op["op"])}: expected error ${expect}, got ${code}: ${(e as Error).message}`);
        }
        throw e;
      }
    }
  }
}

function loadVectors(dir: string): VectorFile[] {
  const paths: string[] = [];
  const stack = [dir];
  while (stack.length > 0) {
    const d = stack.pop()!;
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, entry.name);
      if (entry.isDirectory()) stack.push(p);
      else if (p.endsWith(".json")) paths.push(p);
    }
  }
  paths.sort();
  return paths.map((p) => {
    const raw = fs.readFileSync(p, "utf8");
    try {
      return JSON.parse(raw) as VectorFile;
    } catch (e) {
      throw new Error(`${p}: ${(e as Error).message}`);
    }
  });
}

/** The integration_test pattern: a probed free port and a db path that
 * does not exist (the server auto-creates it as aikoql-v2). */
async function spawnServer(
  bin: string,
  token: string,
): Promise<{ child: ChildProcess; addr: string; dir: string }> {
  const probe = net.createServer();
  await new Promise<void>((resolve, reject) => {
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", resolve);
  });
  const port = (probe.address() as net.AddressInfo).port;
  await new Promise<void>((resolve) => probe.close(() => resolve()));

  const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), "conformance-"));
  const db = path.join(dir, "db.aikoql"); // does not exist → auto-create (aikoql-v2)
  const addr = `127.0.0.1:${port}`;
  // Stdio stays ignore: an inherited pipe would let an orphaned server
  // hold a pipe-capturing parent open on failure.
  const child = spawn(bin, ["serve", db, "--listen", addr, "--tcp-token", `${token}::admin`], {
    stdio: "ignore",
    windowsHide: true,
  });
  const deadline = Date.now() + 15_000;
  for (;;) {
    const up = await new Promise<boolean>((resolve) => {
      const s = net.connect(port, "127.0.0.1");
      s.once("connect", () => {
        s.destroy();
        resolve(true);
      });
      s.once("error", () => resolve(false));
    });
    if (up) return { child, addr, dir };
    if (Date.now() >= deadline) {
      child.kill();
      await fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
      throw new Error(`server did not come up on ${addr}`);
    }
    await sleep(50);
  }
}

async function run(
  bin: string,
  vectorsDir: string,
  protocolDir: string,
  token: string,
): Promise<void> {
  const { child, addr, dir } = await spawnServer(bin, token);
  try {
    const r = new Runner(addr, token);
    let vectorsRun = 0;
    let opsRun = 0;
    for (const dirName of [protocolDir, vectorsDir]) {
      for (const vf of loadVectors(dirName)) {
        await r.runVector(vf.operations).catch((e: Error) => {
          throw new Error(`${vf.name}: ${e.message}`);
        });
        vectorsRun += 1;
        opsRun += vf.operations.length;
        console.log(`  ok ${vf.name} (${vf.operations.length} ops)`);
      }
    }
    console.log(`sdk-conformance (typescript): ${vectorsRun} vectors, ${opsRun} ops — all passed`);
  } finally {
    // Kills the server and sweeps its temp dir on EVERY exit path.
    child.kill();
    await new Promise<void>((resolve) => child.once("exit", () => resolve()));
    await fs.promises.rm(dir, { recursive: true, force: true }).catch(() => {});
  }
}

async function main(): Promise<void> {
  let bin = "";
  let vectorsDir = "";
  let protocolDir = "";
  let token = "conformance";
  for (let i = 2; i < process.argv.length; i++) {
    const a = process.argv[i]!;
    if (a === "--bin") bin = process.argv[++i] ?? "";
    else if (a === "--vectors") vectorsDir = process.argv[++i] ?? "";
    else if (a === "--protocol") protocolDir = process.argv[++i] ?? "";
    else if (a === "--token") token = process.argv[++i] ?? "";
    else {
      process.stderr.write(`unknown arg: ${a}\n`);
      process.exit(1);
    }
  }
  try {
    await run(bin, vectorsDir, protocolDir, token);
  } catch (e) {
    process.stderr.write(`sdk-conformance (typescript): ${(e as Error).message}\n`);
    process.exit(1);
  }
}

void main();
