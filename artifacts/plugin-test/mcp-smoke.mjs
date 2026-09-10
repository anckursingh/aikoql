// MCP stdio smoke: spawn the released binary, drive initialize -> tools/list -> remember -> aikoql query.
// Newline-delimited JSON-RPC, per the MCP stdio transport.
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";

const DB = "./mcp-smoke-kb";
rmSync(DB, { recursive: true, force: true });

// AIKOQL_ARGS: override the serve arguments — used to replay the plugin
// manifest's exact spawn (no db path, pinned metrics addr).
const BIN = process.env.AIKOQL_BINARY;
const baseArgs = process.env.AIKOQL_ARGS
  ? process.env.AIKOQL_ARGS.split(" ")
  : ["serve", DB, "--metrics-addr", "127.0.0.1:0"];
const child = BIN
  ? spawn(BIN, baseArgs, { stdio: ["pipe", "pipe", "pipe"] })
  : spawn("npx", ["-y", "aikoql-mcp@0.1.19", ...baseArgs], {
      stdio: ["pipe", "pipe", "pipe"],
      shell: true,
    });

let buf = "";
const pending = new Map();

function send(id, method, params) {
  const line = JSON.stringify({ jsonrpc: "2.0", id, method, params });
  child.stdin.write(line + "\n");
}

function reply(id, timeoutMs = 20000) {
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
    try {
      msg = JSON.parse(line);
    } catch {
      console.log("NON-JSON STDOUT:", line.slice(0, 300));
      continue;
    }
    console.log("STDOUT LINE:", line.slice(0, 300));
    if (msg.id !== undefined && pending.has(msg.id)) {
      const cb = pending.get(msg.id);
      pending.delete(msg.id);
      cb(msg);
    }
  }
});

child.stderr.on("data", (c) => console.log("STDERR:", c.toString().trim().slice(0, 200)));

const results = {};

// 1. initialize
const initP = reply(1);
send(1, "initialize", {
  protocolVersion: "2024-11-05",
  capabilities: {},
  clientInfo: { name: "mcp-smoke", version: "0.0.0" },
});
const init = await initP;
results.server = { name: init.serverInfo?.name, version: init.serverInfo?.version };
console.log("initialize OK:", JSON.stringify(results.server));

// 2. initialized notification (must be the first client message after initialize)
send(null, "notifications/initialized", {});

// 3. tools/list
send(2, "tools/list", {});
const tools = await reply(2);
results.toolCount = tools.tools.length;
console.log("tools/list OK:", tools.tools.length, "tools ->", tools.tools.map((t) => t.name).join(", "));

// 4. remember a note
const rememberTool = tools.tools.find((t) => t.name.toLowerCase().includes("remember"));
if (!rememberTool) throw new Error("no remember tool");
send(3, "tools/call", {
  name: rememberTool.name,
  arguments: { type_name: "Note", properties: { body: "plugin smoke test" } },
});
const remembered = await reply(3);
console.log("remember OK:", remembered.content?.[0]?.text?.slice(0, 120) ?? JSON.stringify(remembered).slice(0, 120));

// 5. query it back
const qTool = tools.tools.find((t) => t.name.toLowerCase().includes("aikoql") || t.name.toLowerCase().includes("query"));
if (!qTool) throw new Error("no query tool");
send(4, "tools/call", { name: qTool.name, arguments: { query: "MATCH Note RETURN *" } });
const qres = await reply(4);
const qtext = JSON.stringify(qres.content ?? qres);
console.log("query OK, contains note:", qtext.includes("plugin smoke test"));
if (!qtext.includes("plugin smoke test")) throw new Error("note not found in query result");

child.kill();
console.log("PASS: full round-trip");
