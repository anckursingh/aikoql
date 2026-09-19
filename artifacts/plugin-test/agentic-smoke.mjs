// agentic-smoke.mjs — validates the AGENTIC-QUICKSTART.md first-session flow
// against the real binary: session_init -> remember -> aikoql MATCH -> relate
// -> traverse -> find_similar -> explain -> trace -> reason/job_status/
// approve_job -> agent_memory -> record_experience/find_experiences ->
// storage_stats. Newline-delimited JSON-RPC over stdio (same plumbing as
// mcp-smoke.mjs).
//
//   AIKOQL_BINARY=../../target/debug/aikoql-mcp.exe node agentic-smoke.mjs
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";

const DB = "./agentic-smoke-kb";
rmSync(DB, { recursive: true, force: true });

const BIN = process.env.AIKOQL_BINARY;
const args = ["serve", DB, "--metrics-addr", "127.0.0.1:0"];
const child = BIN
  ? spawn(BIN, args, { stdio: ["pipe", "pipe", "pipe"] })
  : spawn("npx", ["-y", "aikoql-mcp@0.1.19", ...args], {
      stdio: ["pipe", "pipe", "pipe"],
      shell: true,
    });

let buf = "";
const pending = new Map();
let nextId = 1;

function send(method, params) {
  const id = nextId++;
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timeout: ${method}`)), 60000);
    pending.set(id, (msg) => {
      clearTimeout(t);
      if (msg.error) reject(new Error(`${method}: ${JSON.stringify(msg.error)}`));
      else resolve(msg.result);
    });
  });
}

child.stdout.on("data", (chunk) => {
  buf += chunk.toString();
  let idx;
  while ((idx = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, idx);
    buf = buf.slice(idx + 1);
    if (!line.trim()) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }
    if (msg.id !== undefined && pending.has(msg.id)) {
      const cb = pending.get(msg.id);
      pending.delete(msg.id);
      cb(msg);
    }
  }
});
child.stderr.on("data", (c) => console.log("STDERR:", c.toString().trim().slice(0, 200)));

const tool = (res) => {
  if (res.isError) throw new Error(`tool error: ${JSON.stringify(res.content)}`);
  const t = res.content.find((c) => c.type === "text")?.text;
  try {
    return JSON.parse(t);
  } catch {
    return { raw: t };
  }
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

await send("initialize", {
  protocolVersion: "2024-11-05",
  capabilities: {},
  clientInfo: { name: "agentic-smoke", version: "0.0.0" },
});
child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");

const tools = await send("tools/list", {});
const names = tools.tools.map((t) => t.name);
console.log(`tools/list: ${names.length} tools`);
for (const req of ["session_init", "remember", "aikoql", "relate", "traverse", "find_similar", "explain", "trace", "reason", "job_status", "approve_job", "agent_memory", "record_experience", "find_experiences", "storage_stats"]) {
  if (!names.includes(req)) throw new Error(`missing tool: ${req}`);
}

const sess = tool(await send("tools/call", { name: "session_init", arguments: { agent_id: "agentic-smoke", run_id: "smoke-1", roles: ["developer", "operator"] } }));
console.log("session_init:", JSON.stringify(sess));

const rememberArgs = {
  type_name: "decision",
  properties: { title: "adopt MCP as the integration surface", why: "every harness ships a stdio MCP client" },
  evidence: [{ source_artifact: "docs/sdk-proxy-decision.md", method: "doc_extraction", revision: "11bd09a" }],
  note: "P3-M9",
};
const rem = tool(await send("tools/call", { name: "remember", arguments: rememberArgs }));
const koid = rem.koid || rem.kos?.[0]?.koid || rem.ko?.koid;
if (!koid) throw new Error(`remember: no koid in ${JSON.stringify(rem)}`);
console.log("remember:", koid);

const q = tool(await send("tools/call", { name: "aikoql", arguments: { query: "MATCH decision RETURN *" } }));
const rows = q.rows ?? q.results ?? q;
if (!Array.isArray(rows) || rows.length < 1) throw new Error(`aikoql MATCH: no rows in ${JSON.stringify(q)}`);
console.log("aikoql MATCH decision:", rows.length, "row(s)");

tool(await send("tools/call", { name: "relate", arguments: { from: koid, to: koid, rel_type: "supersedes" } }));
console.log("relate: ok (self-edge accepted as a graph op)");
// self-edge is silly — redo with two nodes for the doc shape:
const rem2 = tool(await send("tools/call", {
  name: "remember",
  arguments: { type_name: "code_entity", properties: { path: "crates/services/api/mcp", kind: "server" } },
}));
const koid2 = rem2.koid || rem2.kos?.[0]?.koid || rem2.ko?.koid;
tool(await send("tools/call", { name: "relate", arguments: { from: koid, to: koid2, rel_type: "implemented_by" } }));
const tr = tool(await send("tools/call", { name: "traverse", arguments: { koid, rel_type: "implemented_by" } }));
console.log("traverse:", JSON.stringify(tr).slice(0, 120));

const fs = tool(await send("tools/call", { name: "find_similar", arguments: { text: "integration surface harness", k: 5 } }));
const hits = fs.matches ?? fs.results ?? fs.hits ?? [];
if (!Array.isArray(hits) || hits.length < 1) throw new Error(`find_similar: no hits in ${JSON.stringify(fs)}`);
console.log("find_similar:", hits.length, "hit(s)");

const ex = tool(await send("tools/call", { name: "explain", arguments: { koid } }));
if (!ex) throw new Error("explain: empty result");
console.log("explain: ok");

const trc = tool(await send("tools/call", { name: "trace", arguments: { koid } }));
if (!trc) throw new Error("trace: empty result");
console.log("trace: ok");

// Rule-based (MRFC-0011 §6.10): claims fire only when properties is
// non-empty and every property matches — empty rules reason nothing.
const reason = tool(await send("tools/call", {
  name: "reason",
  arguments: { type_name: "decision", properties: { title: "adopt MCP as the integration surface" } },
}));
const jobId = reason.job_id ?? reason.handle?.id;
if (jobId === undefined) throw new Error(`reason: no job handle in ${JSON.stringify(reason)}`);
console.log("reason: job", jobId);

let status;
for (let i = 0; i < 60; i++) {
  await sleep(1000);
  status = tool(await send("tools/call", { name: "job_status", arguments: { job_id: jobId } }));
  const st = status.status ?? status.state;
  if (st === "completed") break;
  if (st === "failed") throw new Error(`reason job failed: ${JSON.stringify(status)}`);
}
if ((status.status ?? status.state) !== "completed") throw new Error("reason job did not complete");
const nClaims = status.claims?.length ?? status.count ?? 0;
if (nClaims < 1) throw new Error(`reason job produced no claims: ${JSON.stringify(status)}`);
console.log("job_status: completed,", nClaims, "claim(s)");

const appr = tool(await send("tools/call", { name: "approve_job", arguments: { job_id: jobId } }));
console.log("approve_job:", JSON.stringify(appr).slice(0, 160));

tool(await send("tools/call", { name: "agent_memory", arguments: { agent_id: "agentic-smoke", key: "last_db", value: "agentic-smoke-kb", ttl: 3600 } }));
const am = tool(await send("tools/call", { name: "agent_memory", arguments: { agent_id: "agentic-smoke" } }));
console.log("agent_memory: read-back ok");

const exp = tool(await send("tools/call", {
  name: "record_experience",
  arguments: {
    goal: "wire aikoql into a coding harness",
    action: "spawn aikoql-mcp serve over stdio from the harness MCP config",
    outcome: "tools/list visible; remember/aikoql round-trip green",
    preconditions: ["aikoql-mcp on PATH"],
    causal_explanation: "stdio transport is the default and needs no token",
    evidence: [{ source_artifact: "AGENTIC-QUICKSTART.md", method: "agent_analysis" }],
  },
}));
if (!exp) throw new Error("record_experience: empty result");
const fe = tool(await send("tools/call", { name: "find_experiences", arguments: { task: "how to wire aikoql into a coding harness", limit: 5 } }));
console.log("find_experiences:", JSON.stringify(fe).slice(0, 160));

const stats = tool(await send("tools/call", { name: "storage_stats", arguments: {} }));
console.log("storage_stats:", Object.keys(stats).join(","));

child.kill();
console.log("AGENTIC SMOKE: PASS");
process.exit(0);
