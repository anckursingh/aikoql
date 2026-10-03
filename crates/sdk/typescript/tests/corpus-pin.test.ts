// The §16 pin: the cross-language golden corpus spec exists at
// sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.
// Removing a case id is a detected coverage loss; a column mismatch is a
// wire-behavior drift (or an undocumented divergence — document it in the
// spec's note and re-stamp). Real primitives run where they exist
// (parseVersion/classifyID/rpcError/streamNotify); the request verdicts
// are restated inline at their client.ts lines.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { classifyID, parseVersion, rpcError, streamNotify } from "../src/client.ts";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CORPUS = path.resolve(__dirname, "..", "..", "..", "..", "sdk-fuzz-corpus", "corpus.json");

const CASE_IDS = [
  "corp-v01", "corp-v02", "corp-v03", "corp-v04", "corp-v05",
  "corp-e01", "corp-e02", "corp-e03", "corp-e04", "corp-e05",
  "corp-r01", "corp-r02", "corp-n01",
  "corp-s01", "corp-s02", "corp-s03",
  "corp-c01", "corp-c02", "corp-c03", "corp-c04",
];
const SURFACES = ["version", "rpc_error", "response_id", "nonfinite", "notify", "correlation"];

function load(): { cases: Array<{ id: string; surface: string; input: unknown; expected: Record<string, unknown> }> } {
  let raw: string;
  try {
    raw = readFileSync(CORPUS, "utf8");
  } catch (err) {
    throw new Error(
      `the §16 corpus is absent (${CORPUS}): the D-16 golden-corpus slice is missing — ${String(err)}`,
    );
  }
  return JSON.parse(raw) as never;
}

function classify(surface: string, input: unknown): unknown {
  switch (surface) {
    case "version":
      return parseVersion(input as string);
    case "rpc_error": {
      const frame = JSON.parse(input as string) as { error?: { code?: unknown; message?: string } };
      // The real error renderer: rpcError + the McpError ctor decoration.
      const e = rpcError(frame.error as { code: unknown; message: string });
      return { code: e.code, message: e.message };
    }
    case "response_id":
    case "nonfinite": {
      // The request loop's verdict, restated (client.ts ~:300-315, want=1):
      // non-numeric ids skip (JSON.parse throws on garbage → skip), and a
      // numeric id goes through classifyID.
      let frame: { id?: unknown };
      try {
        frame = JSON.parse(input as string) as { id?: unknown };
      } catch {
        return "skip";
      }
      const rid = frame.id;
      if (typeof rid !== "number") return "skip";
      return classifyID(1, rid);
    }
    case "notify": {
      // The real primitive: streamNotify (client.ts) — the same verdict
      // the aikoqlStream loop runs; ts yields the raw params object as-is.
      const frame = JSON.parse(input as string) as Record<string, unknown>;
      const p = streamNotify(frame, "s1");
      if (p === null) return { verdict: "skip", pair: null };
      return { verdict: "yield", pair: p };
    }
    case "correlation": {
      const pair = input as { want: number; got: number };
      return classifyID(pair.want, pair.got);
    }
  }
  throw new Error(`unknown surface ${surface}`);
}

test("the §16 corpus pin", () => {
  const spec = load();
  const byId = new Map(spec.cases.map((c) => [c.id, c]));
  for (const id of CASE_IDS) {
    assert.ok(byId.has(id), `case ${id} is gone from the §16 corpus — a removed case is a coverage loss`);
  }
  const haveSurfaces = new Set(spec.cases.map((c) => c.surface));
  for (const s of SURFACES) {
    assert.ok(haveSurfaces.has(s), `surface ${s} has no cases in the §16 corpus`);
  }
  for (const c of spec.cases) {
    assert.ok("typescript" in c.expected, `case ${c.id} has no typescript column in the §16 corpus`);
    const got = classify(c.surface, c.input);
    const want = c.expected.typescript;
    assert.deepEqual(got, want, `case ${c.id} (${c.surface}): typescript column drift — got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
  }
});
