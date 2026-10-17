// D-13: the shared conformance runner, typescript arm (§7, §23). This
// test pins only the CLI contract — the vectors carry the semantics.
// Mirrors crates/sdk/rust/tests/conformance.rs.

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import test from "node:test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Native→"bash" spawns the WSL shim on Windows (its PATH has no cargo/
// node); where.exe is a plain PATH walk and finds Git's bash. Prefer its
// Git hit; plain "bash" elsewhere (CI Linux).
async function bashExe(): Promise<string> {
  if (process.platform !== "win32") return "bash";
  try {
    const { stdout } = await new Promise<{ stdout: string }>((resolve, reject) =>
      execFile("where.exe", ["bash"], { encoding: "utf8" }, (err, stdout) =>
        err ? reject(err) : resolve({ stdout }),
      ),
    );
    const hits = stdout
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter(Boolean);
    return hits.find((h) => h.toLowerCase().includes("git")) ?? hits[0] ?? "bash";
  } catch {
    return "bash";
  }
}

function run(args: string[], env: Record<string, string>): Promise<{ code: number; out: string }> {
  return new Promise((resolve, reject) => {
    execFile(args[0]!, args.slice(1), { encoding: "utf8", env }, (err, stdout, stderr) => {
      if (err && (err as { code?: number }).code === undefined) reject(err);
      else resolve({ code: (err as { code?: number } | null)?.code ?? 0, out: `${stdout}${stderr}` });
    });
  });
}

test("sdk-conformance --language typescript", async () => {
  const script = path.resolve(__dirname, "..", "..", "..", "..", "scripts", "sdk-conformance.sh");
  // crates/sdk/typescript/tests → repo root is four levels up.
  assert.ok(
    (await import("node:fs")).existsSync(script),
    `sdk-conformance runner missing at ${script} — D-13 RED`,
  );
  const bin = process.env.AIKOQL_MCP_BIN;
  if (!bin) {
    process.stderr.write("AIKOQL_MCP_BIN not set — real-server conformance skipped\n");
    return;
  }
  const { code, out } = await run(
    [await bashExe(), script, "--language", "typescript"],
    { ...process.env, AIKOQL_MCP_BIN: bin },
  );
  assert.equal(code, 0, `sdk-conformance --language typescript failed (exit ${code}):\n${out}`);
});
