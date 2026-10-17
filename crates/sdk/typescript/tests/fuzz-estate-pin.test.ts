// D-16 §10: the TS fuzz estate's own pin (the F-04 pattern — a removed
// target is a detected coverage loss, never silent). The estate lives in
// tests/fuzz-estate.test.ts: fast-check property tests over the frozen
// mirrors (version parse, error mapping, id correlation, frame cap,
// envelope decode) plus the §17 command model
// (DISCONNECTED→CONNECTED→INITIALIZED→TRANSACTION→STREAMING→CLOSED).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import test from "node:test";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const estate = path.join(__dirname, "fuzz-estate.test.ts");

// The five property tests and the §17 model, by name.
const pins = [
  "the version-parse property",
  "the error-mapping property",
  "the id-correlation property",
  "the frame-cap property",
  "the envelope property",
  "the §17 command model",
];

test("the fuzz-estate pin", () => {
  const src = readFileSync(estate, "utf8");
  for (const pin of pins) {
    assert.ok(src.includes(pin), `§10 fuzz target missing: ${pin}`);
  }
});
