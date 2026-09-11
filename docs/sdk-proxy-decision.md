# P3-M9 — SDK & proxy surface decision (IMPLEMENTATION-PLAN-PHASE3 §72–73)

Date: 2026-09-11. One page. The blessed integration surface is the MCP server — published, versioned, CI-smoked, and dogfooded per milestone (npm trusted publishing in release.yml; tarball smoke job in ci.yml). Every other client surface below is judged against that standard: **adopt** = workspace member + CI + versioned releases; otherwise **delete**. Deletions land as one commit with zero dangling references; the dependency-DAG CI job is the regression. Only kept surfaces get TDD.

## Evidence (2026-09-11, branch feature/storage-enhancements-phase3)

| Surface | Lines | Tests | CI | Release/publish | Workspace member | In-repo consumers |
|---|---|---|---|---|---|---|
| `crates/sdk/go` (Go) | 222 (1 file) | none | none | none (go.mod, no version) | no | website/docs/sdk/go.md |
| `crates/sdk/typescript` (TS) | 233 (1 file) | script only — points at `dist/test.js`, **no `dist/` exists** | none | none (package.json 0.1.0, stale vs 0.1.19) | no | QUICKSTART.md:221, examples/hello-agent.ts, website (index.html tabs, docs/sdk/typescript.md) |
| `crates/sdk/java` (Java) | 1 file | none | none | none (unversioned) | no | QUICKSTART.md:245, website/docs/sdk/java.md |
| `crates/sdk/python` (Python) | PyO3/maturin | **17 tests** (5 sdk + 8 mcp_client + 4 adapters); mcp_client spawns the real `aikoql-mcp` binary | none | none (no PyPI job; pyproject **0.1.0 stale** vs workspace 0.1.19; crate is `version.workspace` = 0.1.19 — the gap is the manifest only) | **yes** | E2E (SE2-M41: "Python SDK default flipped", full E2E green) |
| `crates/cluster/proxy` | 306 (main.rs, serde_json only) | none | none | none (no packaging) | yes | none (no federation demand anywhere) |

## Decisions

1. **Go SDK — DELETE.** Outside workspace, unversioned, zero tests, zero CI, zero consumers. MCP (stdio/REST) is the blessed surface; an agent needing Go speaks MCP.
2. **TypeScript SDK — DELETE.** Same evidence class; its only repo reference is a QUICKSTART example that imports a file the package cannot even build (no dist). The published, CI-smoked npm surface is `npm-publish/` (the MCP plugin) — agents in JS get the plugin, not an SDK. QUICKSTART.md:221 becomes an MCP-client example.
3. **Java SDK — DELETE.** Not named by §72 but evidence-identical to Go/TS (one unbuilt file, no tests, no CI, no consumers).
4. **Cluster proxy — DELETE.** No tests, no CI, no packaging, no federation demand, zero dependents (`serde_json` only). A shard router is a one-week rebuild on real federation demand; keeping 306 lines of unexercised routing code as a spec is fiction. The crate leaves the workspace members list; the dependency-DAG job + `cargo build --workspace` are the regression.
5. **Python SDK — ADOPT.** Workspace member, real tests (including contract tests against the real MCP binary), used by the E2E chain. Kept surfaces get TDD: **sdk001** = version-parity contract — `aikoql.__version__` equals the workspace version parsed from the root Cargo.toml (RED today: no `__version__` attribute exists and pyproject pins 0.1.0). Fix: pyproject `dynamic = ["version"]` (maturin reads the crate's `version.workspace` = single source of truth) + the PyO3 module exports `__version__` from `CARGO_PKG_VERSION`. **PyPI publish job** added to release.yml beside the npm job (maturin build + trusted publishing), gated on the same tag/version-match check.
   - The adoption TDD also caught real drift: P3-M1 (TCP token auth) had silently broken all 8 mcp_client contract tests. Fixed the client (token rides initialize params; `session_init` no longer sends agent_id on TCP — identity is server-assigned) and the fixture (new CLI shape + `--tcp-token`). Suite now 18/18 green against the real binary — the strongest evidence that a kept surface needs its contract tests in CI.

## Re-adopt triggers

The deleted surfaces are recoverable from git history. Rebuild them — against
the then-current MCP contract, with CI + contract tests + versioned releases —
when one of these fires:

1. A real external demand signal (consumer, issue, or customer) for a Go/TS/Java driver, or
2. The federation requirement that makes a shard router real, or
3. `docs/first-class-db-roadmap.md` Phase 4 (driver program) starts.

Until then, MCP + the Python SDK is the right-sized surface.

## Result (2026-09-11)

User decision: **delete, plus a future plan for primary-DB positioning** →
`docs/first-class-db-roadmap.md`. All four deletions landed in this commit
(workspace member removed, CI DAG pin added, QUICKSTART/website/docs updated
to the MCP surface). Python stays adopted with sdk001 green.
