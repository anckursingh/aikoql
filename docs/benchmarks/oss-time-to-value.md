# Wave 3.1 OSS time-to-value (W31-OSS-001)

The spec's contract: a fresh developer receives **only** the README,
the quickstart, and the examples, then walks seven tasks — install,
start, ingest, query, add a second source, create a knowledge-backed
agent, debug a failure. Measured: time, completion rate, documentation
failures, support interventions. Targets must come from baseline
observations, never invented.

## Baseline observations (this machine, debug build)

| Task | Mechanical leg | Done | µs |
|---|---|---|---|
| install | released-binary path: binary present (no install step) | yes | — |
| start | spawn server + MCP initialize | yes | 240 373 |
| ingest | `remember` (quickstart's hello note) | yes | 5 573 |
| query | `find_similar` finds the note | yes | 3 613 |
| add second source | second `remember` + both recall | yes | 10 567 |
| knowledge-backed agent | `session_init` + agent `remember` + agent recall | yes | 9 947 |
| debug | `explain` + `trace` on the ingested KO | yes | 5 125 |

- Completion rate: **7/7** — pinned in `wave31_oss.rs` (every leg
  asserted, not just printed).
- Whole-flow wall time: 1.37 s of mechanical legs (debug-build
  profile; the latency law applies — times are printed, never
  thresholded).
- From-source baseline: `cargo build -p aikoql-mcp` (debug) observed
  at **12m49s** on this machine. The quickstart's 5-second start
  assumes the released binary path (`npm i -g aikoql-mcp` or GitHub
  Releases), where install has no build step.

## Documentation failures found and closed (the TDD fix leg)

Each is a real failure — the measurement ran first (RED), the artifact
shipped after:

1. **README.md did not exist** — a fresh developer arriving at the
   repo root had no entry point. Created: name, one-line scope,
   pointers to the quickstart, examples, docs, releases.
2. **examples/ did not exist** — the spec hands a fresh developer
   "examples"; the repo shipped none. Created:
   `examples/hello-agent.ts`, the seven-step flow in TypeScript
   against the bundled SDK (tool names pinned against the real
   registry by the artifact-law test).
3. **The MCP rate limit was undocumented** — the server defaults to
   120 calls/min (PRR-4); a busy agent loop hits that immediately and
   no artifact said so. One config section added to QUICKSTART.md.

Pinned law (`w31_oss_002_onboarding_artifact_laws`): the three
artifacts exist at the mandated paths, the README leads to the other
two, the quickstart literally covers each of the seven tasks, and the
shipped example exercises tools the server actually has.

## Support interventions

0 after the three fixes above. During the measurement the only
interventions the flow would have required were the three missing
artifacts/knobs — the definition the spec gives ("support
intervention") is exactly a step the three artifacts do not cover.

## Targets, derived from the baseline

- **Mechanical completion**: 7/7 with zero documentation failures and
  zero support interventions — the baseline already achieves it; the
  pinned test keeps it true.
- **Wall-clock target for a human**: NOT set. The baseline only
  bounds the floor (≈1.4 s mechanical + 0 s install via released
  binary, 12m49s via from-source build). A real human-time target
  needs a real fresh developer; none was available for this
  measurement. Recorded as an unknown rather than invented
  (unknown.md).
