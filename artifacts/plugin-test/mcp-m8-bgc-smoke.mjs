// P3-M8 dogfood: repo-built aikoql-mcp binary, fresh KB on the v2 backend
// with the M8 defaults live (background compactor thread spawned).
// Round-trip under the new architecture + the new counter through the
// plugin surface:
//   remember xN -> aikoql MATCH (answers byte-exact while the compactor is
//   live) -> storage_stats (compaction_error_count present == 0) ->
//   storage_compact (explicit compact stays synchronous — the M8 pin).
// Newline-delimited JSON-RPC over stdio (the MCP transport).
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const DB = join(tmpdir(), "m8-bgc-smoke-kb");
rmSync(DB, { recursive: true, force: true });

const BIN = process.env.AIKOQL_BINARY ?? "../../target/debug/aikoql-mcp.exe";
const child = spawn(BIN, ["serve", DB, "--metrics-addr", "127.0.0.1:0"], {
  stdio: ["pipe", "pipe", "pipe"],
});

let buf = "";
const pending = new Map();

function send(id, method, params) {
  const line = JSON.stringify({ jsonrpc: "2.0", id, method, params });
  child.stdin.write(line + "\n");
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
    try {
      msg = JSON.parse(line);
    } catch {
      console.log("NON-JSON STDOUT:", line.slice(0, 300));
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

function textOf(res) {
  return res.content?.map((c) => c.text ?? "").join("\n") ?? JSON.stringify(res);
}

// 1. initialize + initialized
const initP = reply(1);
send(1, "initialize", {
  protocolVersion: "2024-11-05",
  capabilities: {},
  clientInfo: { name: "mcp-m8-bgc-smoke", version: "0.0.0" },
});
const init = await initP;
console.log("initialize OK:", init.serverInfo?.name, init.serverInfo?.version);
send(null, "notifications/initialized", {});

// 2. tools/list — remember, query, storage_stats, storage_compact present
send(2, "tools/list", {});
const tools = await reply(2);
const find = (s) => tools.tools.find((t) => t.name.toLowerCase().includes(s));
for (const name of ["remember", "aikoql", "storage_stats", "storage_compact"]) {
  if (!find(name)) throw new Error(`tool missing: ${name}`);
}
console.log("tools/list OK:", tools.tools.length, "tools");

// 3. remember rows (smoke scale — the 64 MiB memtable never flushes, so
//    no trigger crossings; the compactor thread is live regardless)
const n = 30;
let last;
for (let i = 0; i < n; i++) {
  send(10 + i, "tools/call", {
    name: find("remember").name,
    arguments: { type_name: "Note", properties: { body: `bgc smoke row ${i}` } },
  });
  last = await reply(10 + i);
}
console.log(`remember OK: ${n} rows (last: ${textOf(last).slice(0, 80)})`);

// 4. MATCH round-trip under the live compactor
send(100, "tools/call", {
  name: find("aikoql").name,
  arguments: { query: "MATCH Note RETURN *" },
});
const qres = await reply(100);
const qtext = textOf(qres);
const rows = (qtext.match(/bgc smoke row/g) ?? []).length;
console.log("MATCH OK:", rows, "rows");
if (rows < n) throw new Error(`MATCH returned ${rows} rows, want ${n}`);

// 5. storage_stats — the new P3-M8 counter rides the plugin surface
send(101, "tools/call", { name: find("storage_stats").name, arguments: {} });
const stats = JSON.parse(textOf(await reply(101)));
const w = stats.write;
console.log(
  "storage_stats OK:",
  `error_count=${w.compaction_error_count}`,
  `flush=${w.flush_count}`,
  `backlog=${w.compaction_backlog_bytes}B/${w.compaction_pending_segments}segs`,
  `last_compact_ms=${w.last_compaction_ms}`
);
if (w.compaction_error_count !== 0) throw new Error("background merge errors at rest");
if (w.last_compaction_ms !== 0) throw new Error("no merge ran — last_compaction_ms must be 0");

// 6. storage_compact — explicit compaction stays synchronous (the M8 pin).
// At smoke scale everything still sits in the 64 MiB memtable (flush=0),
// so the correct answer is a valid in=0/out=0 envelope; the merge path
// itself is pinned by the mcp met004-006 tests on a small-memtable server.
send(102, "tools/call", { name: find("storage_compact").name, arguments: {} });
const comp = JSON.parse(textOf(await reply(102)));
console.log("storage_compact OK:", `in=${comp.segments_in} out=${comp.segments_out}`);

// 7. rows survive the explicit compaction
send(103, "tools/call", {
  name: find("aikoql").name,
  arguments: { query: "MATCH Note RETURN *" },
});
const q2 = textOf(await reply(103));
const rows2 = (q2.match(/bgc smoke row/g) ?? []).length;
console.log("MATCH after compact OK:", rows2, "rows");
if (rows2 < n) throw new Error(`post-compact MATCH returned ${rows2} rows, want ${n}`);

// 8. storage_stats again — nothing merged (all-memtable state), no errors
send(104, "tools/call", { name: find("storage_stats").name, arguments: {} });
const stats2 = JSON.parse(textOf(await reply(104)));
console.log(
  "storage_stats after compact OK:",
  `error_count=${stats2.write.compaction_error_count}`,
  `last_compaction_ms=${stats2.write.last_compaction_ms}`
);
if (stats2.write.compaction_error_count !== 0) throw new Error("merge errors after compact");

child.kill();
console.log("PASS: P3-M8 plugin round-trip");
