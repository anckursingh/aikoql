# Aikoql for Agentic AI Developers

A quickstart for coding agents — Claude Code, Codex, Cursor, and any
harness that speaks MCP. Everything below is validated against the real
binary by `artifacts/plugin-test/agentic-smoke.mjs` (97 tools, full loop
green).

---

## Why a coding agent needs Aikoql

Coding agents are amnesiac: every session restarts with nothing but the
repo. Git holds *what changed*, not *why*, not *what we know*. Aikoql is a
knowledge database built for agents — every fact carries provenance,
evidence, epistemic status, temporal validity, and an audit chain. A vector
DB gives you similar text; Aikoql gives you **believed, attributable,
versioned knowledge you can query and prove**.

| Agent need | Aikoql answer |
|---|---|
| Remember decisions across sessions | `remember` a `decision` KO with evidence → recall next session via `find_similar` / `MATCH` |
| Know *why* a fact is believed | `explain` — provenance, source artifact, method, confidence |
| See the history of a fact | `trace` — every version + event; `aikoql ... AS_OF` for "as of commit X" |
| Prove an audit trail | `prove`, `audit_report` — SHA-256 hash-chained journal |
| Track hypotheses until tested | epistemic lifecycle: `remember` (draft) → `verify` → `evolve` to `verified`; contradictions via `contradict`/`resolve_conflict` |
| Reason without blocking the session | `reason`/`infer`/`predict` are **async jobs** — poll `job_status`, commit via `approve_job` (nothing enters the store unapproved) |
| Reuse past task outcomes | `record_experience` / `find_experiences` — goal/action/outcome, TTL-bounded, ACL-scoped |
| Per-agent scratch memory | `agent_memory` — key/value with TTL per agent id |
| Session handoff | `summarize_conversation` — deterministic 7-bucket extraction (never invents facts) |
| Model the codebase | `relate`/`traverse` — module→function→test edges; `register_schema` + `constraint_diagnostics` — enforce invariants |
| Private vs shared knowledge | tenants, roles, ACLs — private agent memory vs team knowledge base |

**The surface is MCP.** 97 tools over stdio (local, trusted, no token) or
TCP (shared, token-scoped). Python gets a first-party SDK (`from aikoql
import Agent`); every other language uses its standard MCP client —
`docs/sdk-proxy-decision.md` explains why hand-rolled SDKs were deleted.

## Install

```bash
npm i -g aikoql-mcp            # npm launcher (downloads the right binary)
# or grab the binary from https://github.com/anckursingh/aikoql/releases
# or Docker: ghcr.io/anckursingh/aikoql:0.1.19  (state under /data volume)
```

A fresh path creates an **aikoql-v2** directory (native engine: WAL +
tiered compacted segments). An existing `.redb` file or v1 WAL
auto-detects as its own backend.

## Wire it into your harness

**Claude Code:**

```bash
claude mcp add aikoql -- npx -y aikoql-mcp serve ./kb
```

(A packaged aikoql plugin also exists for Claude Code — it runs this same
server, with the knowledge-base path pinned via `userConfig.KB_PATH`.)

**Codex:**

```bash
codex mcp add aikoql -- npx -y aikoql-mcp serve ./kb
```

**Anything else** (Cursor, Windsurf, VS Code, custom harnesses) — the
portable MCP config block, e.g. `.vscode/mcp.json` / `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "aikoql": {
      "command": "npx",
      "args": ["-y", "aikoql-mcp", "serve", "./kb"]
    }
  }
}
```

**Shared team KB over TCP** (server on a host, many agents/clients):

```bash
aikoql-mcp serve --listen 0.0.0.0:9090 --tcp-token s3cret:team:admin /data/kb
```

TCP auth is fail-closed: the token is `TOKEN[:TENANT[:ROLE1,ROLE2]]` and a
bare `TOKEN` (no roles) **exits 2**. In production use `AIKOQL_TCP_TOKEN`
or `AIKOQL_TCP_TOKEN_FILE` (CLI flags leak into the process list). On TCP,
identity is server-assigned by the token — clients must not send an
`agent_id`; on stdio, the session is trusted and you declare identity with
`session_init`.

## First 10 minutes (validated flow)

Raw MCP calls shown as JSON; your harness's MCP client wraps them as
`mcp__aikoql__remember(...)` etc.

```jsonc
// 1. declare who this session is (stdio)
{"name":"session_init","arguments":{"agent_id":"claude","run_id":"run-123","roles":["developer"]}}
// → {"session":{...},"established":true}

// 2. commit a decision with evidence (method must be one of the enum:
//    ast_extraction, doc_extraction, test_observation, ci_observation,
//    runtime_observation, agent_analysis, llm_inference, human_provided,
//    derivation)
{"name":"remember","arguments":{
  "type_name":"decision",
  "properties":{"title":"adopt MCP as the integration surface","why":"every harness has a stdio MCP client"},
  "evidence":[{"source_artifact":"docs/sdk-proxy-decision.md","method":"doc_extraction","revision":"11bd09a"}],
  "note":"P3-M9"}}
// → {"koid":"01a08f1d44a5…","version":1}

// 3. query it back
{"name":"aikoql","arguments":{"query":"MATCH decision RETURN *"}}
// → rows:[{…}]

// 4. graph: link it to the code it governs, then walk the edge
{"name":"remember","arguments":{"type_name":"code_entity","properties":{"path":"crates/services/api/mcp","kind":"server"}}}
{"name":"relate","arguments":{"from":"<decision koid>","to":"<entity koid>","rel_type":"implemented_by"}}
{"name":"traverse","arguments":{"koid":"<decision koid>","rel_type":"implemented_by"}}
// → {"hits":[{"koid":"…","depth":1,"rel_type":"implemented_by"}]}

// 5. hybrid recall
{"name":"find_similar","arguments":{"text":"integration surface harness","k":5}}
// → matches:[…]   (vector + text, RRF/weighted fusion)

// 6. provenance
{"name":"explain","arguments":{"koid":"<koid>"}}   // why believed, source, method, confidence
{"name":"trace","arguments":{"koid":"<koid>"}}     // versions + events lineage

// 7. async reasoning — reason is a DETERMINISTIC rule engine (type +
//    property filter), not an LLM call. Claims fire only when properties
//    is non-empty and every property matches. Nothing enters the store
//    until approve_job.
{"name":"reason","arguments":{"type_name":"decision","properties":{"title":"adopt MCP as the integration surface"}}}
// → {"job_id":0,"status":"running","next":"poll job_status; approve_job"}
{"name":"job_status","arguments":{"job_id":0}}     // → status:"completed", claims:[1]
{"name":"approve_job","arguments":{"job_id":0}}    // → {"committed":["01a08f1d…"],"count":1}
//    committed as type "decision-claim", origin=reason, epistemic=inferred

// 8. per-agent scratch memory + reusable experience
{"name":"agent_memory","arguments":{"agent_id":"claude","key":"last_db","value":"kb","ttl":3600}}
{"name":"record_experience","arguments":{
  "goal":"wire aikoql into a coding harness",
  "action":"spawn aikoql-mcp serve over stdio from the harness MCP config",
  "outcome":"tools/list visible; remember/aikoql round-trip green",
  "evidence":[{"source_artifact":"AGENTIC-QUICKSTART.md","method":"agent_analysis"}]}}
{"name":"find_experiences","arguments":{"task":"wire aikoql into a coding harness","limit":5}}
```

## Recipes for coding agents

**Durable decision memory.** When you make a consequential call — adopt a
dependency, pick an architecture, reject an option — `remember` a
`decision` KO with `evidence` (the doc/issue/thread). Next session, before
re-deciding: `find_similar` on the topic, `explain` the candidate, check
`trace` for updates. You stop re-litigating.

**Debug fingerprints.** For a nasty failure: `remember` an `error` KO with
the signature as properties, then `relate` it to the fix KO
(`rel_type:"fixed_by"`). A future session hit with the same error calls
`find_similar` on the stack signature — the fix comes back with its
provenance.

**Hypothesis tracking.** Store uncertain findings as drafts (default
state), and move them along the lifecycle: `verify` after a test passes,
`evolve` to `verified`. Conflicting facts are first-class —
`contradict`/`resolve_conflict` record the disagreement instead of
silently overwriting.

**Async reasoning gate.** `reason`/`infer`/`predict` return job handles —
kick one off, keep working, poll `job_status`, and `approve_job` only the
claims you accept. Class-B output never enters the store unapproved
(MRFC-0011 §7 Determinism Law).

**Team KB vs private memory.** Shared org knowledge lives in the KB with
tenants/ACLs; transient per-agent state goes in `agent_memory` (TTL).
`record_experience` with `shared_with` grants specific agents reuse
access.

**Session handoff.** End a session with `summarize_conversation` — the
summary KO carries speaker + message-range provenance into the next
session.

## Test checklist for a harness

Bringing a new harness online (5 min, matches the smoke script):

- [ ] `tools/list` returns 97 tools and contains `remember`, `aikoql`,
      `session_init`, `find_similar`, `explain`, `trace`, `reason`,
      `approve_job`
- [ ] `session_init` establishes identity
- [ ] `remember` → KOID; `aikoql "MATCH <type> RETURN *"` returns the row
- [ ] `relate` + `traverse` round-trip
- [ ] `find_similar` hits the remembered object
- [ ] `explain` and `trace` return provenance for the KOID
- [ ] `reason` (with a property filter) → `job_status` completed →
      `approve_job` commits ≥1 claim
- [ ] `agent_memory` write/read round-trip
- [ ] `record_experience` → `find_experiences` matches it
- [ ] **The real test — persistence:** restart the harness/session, run
      `MATCH` and `find_similar` again. The knowledge is still there, with
      its lineage. That continuity is the product.

Known gotchas (all confirmed, not folklore):

- **Rate limit:** 120 MCP calls/min by default — a busy agent loop hits it
  fast. Raise in `aikoql.toml`:
  `[rate_limit] max_calls_per_minute = 100000`
- **Evidence methods** are the fixed enum above; unknown methods are
  rejected (`unknown evidence method`).
- **`reason` with no `properties` reasons nothing** — the rule filter must
  be non-empty and match.
- **Operator-gated tools** (`storage_stats`, `storage_compact`,
  `storage_checkpoint`, `backup`, `restore`, `execute_program`,
  `invalidate`) need the `operator` role — a `session_init` with only
  `developer` is denied; bare stdio (no session_init) is trusted.
- **TCP tokens need roles** — bare `TOKEN` exits 2.
- **Don't commit the KB** — `./kb` is state, add it to `.gitignore`.

## Ops

- Backups: `backup` → `verify_backup` → `restore`; `list_backups`
- Storage: `storage_stats` (write-path, segments, cache), `storage_compact`,
  `storage_checkpoint`
- Observability: `--metrics-addr 127.0.0.1:9091` → `GET /metrics`
  (Prometheus), `GET /health`, `GET /studio` (web UI — query editor, graph
  explorer, ontology, timeline, provenance)
- Docs pipeline: `document_ingest` / `document_status` / `document_compile`
  (PDF, DOCX, HTML, TXT → knowledge IR)

## Where this is going

`docs/first-class-db-roadmap.md`: phase-gated path from knowledge layer to
first-class primary database (performance parity vs Postgres/Neo4j,
replication/HA, sharding), with first-party drivers returning at Phase 4
when the demand signal exists. Today: one binary, MCP, 97 tools, proven
durability — start with the 10-minute flow above.
