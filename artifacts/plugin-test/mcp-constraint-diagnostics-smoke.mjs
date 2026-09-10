// P3-M5 dogfood: repo-built plugin binary round-trip for the constraint
// engine surface. initialize -> tools/list (constraint_diagnostics +
// register_schema present) -> tools/call (events[] + stats{} shape pinned)
// -> register_schema (Person: advisory check ck_age, enforced unique email,
// advisory cardinality c_member max 2) -> remember age=15 (advisory: write
// succeeds) -> constraint_diagnostics shows the ck_age event with
// mode=Advisory + severity=Warning + koid -> duplicate email blocked
// (enforced unique) -> cardinality breach recorded advisory.
import { spawn } from "node:child_process";
import { rmSync, mkdirSync } from "node:fs";
import path from "node:path";

const DB = "./tmp/dogfood-constraint-kb";
rmSync(DB, { recursive: true, force: true });
mkdirSync("tmp", { recursive: true });

const BIN = path.resolve("target/debug/aikoql-mcp.exe");
const child = spawn(BIN, ["serve", DB, "--metrics-addr", "127.0.0.1:0"], {
  stdio: ["pipe", "pipe", "pipe"],
});

let buf = "";
const pending = new Map();
function send(id, method, params) {
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
}
function reply(id, timeoutMs = 30000) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timeout waiting for id ${id}`)), timeoutMs);
    pending.set(id, (msg) => {
      clearTimeout(t);
      if (msg.error) reject(new Error(`jsonrpc error ${id}: ${JSON.stringify(msg.error)}`));
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
    try { msg = JSON.parse(line); } catch { continue; }
    if (msg.id !== undefined && pending.has(msg.id)) {
      const cb = pending.get(msg.id);
      pending.delete(msg.id);
      cb(msg);
    }
  }
});
child.stderr.on("data", (c) => console.log("STDERR:", c.toString().trim().slice(0, 200)));

let nextId = 0;
async function call(method, params) {
  const id = ++nextId;
  const p = reply(id);
  send(id, method, params);
  return p;
}

const init = await call("initialize", {
  protocolVersion: "2024-11-05",
  capabilities: {},
  clientInfo: { name: "mcp-constraint-smoke", version: "0.0.0" },
});
console.log("initialize:", init.serverInfo?.name, init.serverInfo?.version);
send(null, "notifications/initialized", {});

const tools = await call("tools/list", {});
const names = tools.tools.map((t) => t.name);
for (const t of ["constraint_diagnostics", "register_schema"]) {
  if (!names.includes(t)) throw new Error(`${t} missing from tools/list`);
}
console.log(
  "tools/list: constraint_diagnostics + register_schema present (",
  names.length,
  "tools )",
);

const diag = await call("tools/call", {
  name: "constraint_diagnostics",
  arguments: {},
});
const content = JSON.parse(diag.content[0].text);
if (!Array.isArray(content.events)) throw new Error("missing events[]");
for (const k of ["evaluated", "skipped_disabled", "skipped_unaffected"]) {
  if (typeof content.stats?.[k] !== "number") throw new Error(`missing stats.${k}`);
}
console.log(
  "constraint_diagnostics: events",
  content.events.length,
  "stats",
  JSON.stringify(content.stats),
);

const registered = await call("tools/call", {
  name: "register_schema",
  arguments: {
    type_name: "Person",
    properties: [
      { name: "name", value_type: "Text" },
      { name: "age", value_type: "Int" },
      { name: "email", value_type: "Text" },
    ],
    checks: [
      { name: "ck_age", expr: "age >= 18", mode: "Advisory", severity: "Warning" },
    ],
    uniques: [
      { properties: ["email"], scope: "Type", mode: "Enforced", severity: "Error" },
    ],
    cardinality: [
      { name: "c_member", relationship_type: "member", max: 2, mode: "Advisory", severity: "Warning" },
    ],
  },
});
console.log("register_schema:", JSON.stringify(JSON.parse(registered.content[0].text)));

async function rememberPerson(email, age) {
  const r = await call("tools/call", {
    name: "remember",
    arguments: {
      subject: "smoke-agent",
      type_name: "Person",
      properties: { email, age },
      schema_version: 1,
    },
  });
  return JSON.parse(r.content[0].text).koid;
}

// age 15 violates ck_age but the mode is Advisory → write succeeds.
const violator = await rememberPerson("ann@x.com", 15);
console.log("advisory remember (age 15):", violator.slice(0, 8), "... succeeded");

// Same email under the Enforced unique → blocked. The MCP layer reports
// tool-level failures as a normal result with isError=true and the error
// payload inside content[0].text (not a JSON-RPC rejection).
const dupRes = await call("tools/call", {
  name: "remember",
  arguments: {
    subject: "smoke-agent",
    type_name: "Person",
    properties: { email: "ann@x.com", age: 30 },
    schema_version: 1,
  },
});
const dupPayload = JSON.parse(dupRes.content[0].text);
if (!dupRes.isError || !/uniqueness/i.test(dupPayload.error?.message ?? "")) {
  throw new Error("enforced unique did not block: " + JSON.stringify(dupPayload));
}
console.log("duplicate email blocked:", dupPayload.error.message);

// Cardinality: 3 outbound member rels on the violator, max 2 → advisory event.
for (let i = 0; i < 3; i++) {
  const member = await rememberPerson(`m${i}@x.com`, 20 + i);
  await call("tools/call", {
    name: "relate",
    arguments: { subject: "smoke-agent", from: violator, to: member, rel_type: "member" },
  });
}

const diag2 = await call("tools/call", {
  name: "constraint_diagnostics",
  arguments: {},
});
const content2 = JSON.parse(diag2.content[0].text);
const byConstraint = {};
for (const ev of content2.events) byConstraint[ev.constraint] = ev;
const ageEvt = byConstraint["ck_age"];
const cardEvt = byConstraint["c_member"];
if (!ageEvt || ageEvt.mode !== "Advisory" || ageEvt.severity !== "Warning" || ageEvt.koid !== violator) {
  throw new Error("ck_age advisory event missing or malformed: " + JSON.stringify(ageEvt));
}
if (!cardEvt || cardEvt.mode !== "Advisory" || cardEvt.koid !== violator) {
  throw new Error("c_member cardinality event missing or malformed: " + JSON.stringify(cardEvt));
}
console.log(
  "constraint_diagnostics: ck_age",
  JSON.stringify({ mode: ageEvt.mode, severity: ageEvt.severity, koid: ageEvt.koid.slice(0, 8) }),
  "| c_member",
  JSON.stringify({ mode: cardEvt.mode, koid: cardEvt.koid.slice(0, 8) }),
  "| stats",
  JSON.stringify(content2.stats),
);

child.kill();
console.log("P3-M5 DOGFOOD GREEN");
