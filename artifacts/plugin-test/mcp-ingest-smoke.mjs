// P3-M4 dogfood: repo-built plugin binary round-trip for the INGEST query.
// initialize -> tools/list -> INGEST "<artifact>" COMMIT -> MATCH aikoql:document read-back.
import { spawn } from "node:child_process";
import { rmSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";

const DB = "./tmp/dogfood-ingest-kb";
rmSync(DB, { recursive: true, force: true });
mkdirSync("tmp", { recursive: true });

const ARTIFACT = path.resolve("tmp/dogfood-ingest-artifact.txt");
writeFileSync(ARTIFACT, "P3-M4 dogfood: INGEST lowers to IngestOp and deploys this file.\n");

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

const initP = reply(1);
send(1, "initialize", {
  protocolVersion: "2024-11-05",
  capabilities: {},
  clientInfo: { name: "mcp-ingest-smoke", version: "0.0.0" },
});
await initP;
send(null, "notifications/initialized", {});

send(2, "tools/list", {});
const tools = await reply(2);
const qTool = tools.tools.find((t) => t.name === "aikoql");
if (!qTool) throw new Error("no aikoql tool; tools: " + tools.tools.map((t) => t.name).join(", "));
console.log("tools/list OK:", tools.tools.length, "tools; aikoql tool present");

// INGEST via the plugin's own query tool.
const artifactUri = ARTIFACT.replace(/\\/g, "/");
send(3, "tools/call", {
  name: "aikoql",
  arguments: { query: `INGEST "${artifactUri}" COMMIT`, subject: "system" },
});
const ingest = await reply(3);
const ingestJson = JSON.parse(ingest.content?.[0]?.text ?? "{}");
const doc = ingestJson.results?.[0];
console.log("INGEST OK:", JSON.stringify(doc).slice(0, 200));
if (doc?.type_name !== "aikoql:document") throw new Error("INGEST did not deploy an aikoql:document");
if (doc?.properties?.filename !== "dogfood-ingest-artifact.txt")
  throw new Error("wrong filename: " + JSON.stringify(doc?.properties));
if (doc?.properties?.status !== "ingested") throw new Error("wrong status: " + doc?.properties?.status);
if (!doc?.extensions?.["content_trust"]) throw new Error("content_trust extension missing");

// Read back via the plugin's own query tool — MATCH on the namespaced type.
send(4, "tools/call", {
  name: "aikoql",
  arguments: { query: "MATCH aikoql:document RETURN *", subject: "system" },
});
const readback = await reply(4);
const text = readback.content?.[0]?.text ?? JSON.stringify(readback);
console.log("MATCH aikoql:document contains filename:", text.includes("dogfood-ingest-artifact.txt"));
if (!text.includes("dogfood-ingest-artifact.txt"))
  throw new Error("ingested document not found in MATCH result");

// Cross-check via document_list too.
send(5, "tools/call", { name: "document_list", arguments: { subject: "system" } });
const list = await reply(5);
const listText = list.content?.[0]?.text ?? JSON.stringify(list);
console.log("document_list contains filename:", listText.includes("dogfood-ingest-artifact.txt"));
if (!listText.includes("dogfood-ingest-artifact.txt"))
  throw new Error("ingested document not found in document_list result");

child.kill();
console.log("PASS: INGEST round-trip through repo-built plugin");
