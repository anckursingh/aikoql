// V-02: the default client identity is the package version read from the
// manifest at runtime — the source must not restate it (a literal always
// drifts from package.json on the next bump). RED: the literal is gone,
// the manifest is not yet read.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { MIN_SERVER_VERSION } from "../src/client.ts";
import { dial, respond, scripted } from "./helpers.ts";

test("the default client identity is the package version", async (t) => {
  const pkg = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  ) as { version: string };
  let sent: { version: string };
  const addr = await scripted(t, (req) => {
    if (req.method === "initialize") {
      sent = (req.params as { clientInfo: { version: string } }).clientInfo;
      return [[respond(req.id, MIN_SERVER_VERSION), 0]];
    }
    return [];
  });
  const c = await dial(t, addr);
  await c.initialize();
  assert.equal(sent!.version, pkg.version, "the default identity must be the manifest version");
});
