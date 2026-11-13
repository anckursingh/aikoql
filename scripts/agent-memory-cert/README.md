# Agent-memory certification harness (C1–C8)

A black-box run of AikoQL **as an agent's memory**, against the eight levels
of `docs/AIKOQL-Chatbot-Memory-Knowledge-Certification-Test-Suite.md`. It
drives a real `aikoql-mcp serve` over MCP (the TCP transport an agent uses),
with no LLM in the loop. Each check records PASS, FAIL or GAP (the capability
is not there) and the server's own answer as evidence.

```bash
cargo build -p aikoql-mcp
python scripts/agent-memory-cert/c1_c8.py      # one run -> results.json
python scripts/agent-memory-cert/repeat.py 5   # five runs -> summary.json
```

- `AIKOQL_BIN`: the server binary (default `target/debug/aikoql-mcp`).
- `AIKOQL_CERT_OUT`: the work dir for databases, logs and results (default
  `<tmp>/aikoql-agent-memory-cert`). Nothing is written into the repo.

The server is started with three tenant tokens (`acme` admin, `acme` editor,
`acme` viewer) and one in another tenant (`globex`). It is restarted once
mid-run (CHAT-MEM-003).

## What it covers

29 checks against a 55-section suite. These are the **model-free** checks:
what the memory store itself guarantees. The suite's baselines (LLM-only,
vector RAG, graph RAG) are not run here.

| Level | Checks |
|---|---|
| C1 conversational | same-session, cross-session, survives a restart, explicit memory, ephemeral TTL |
| C2 semantic | direct fact by query, multi-hop traversal, current truth after a change, `AS_OF` history, contradiction recorded, authority resolves it, forget |
| C3 episodic | experience recalled, failed action surfaced, conversation → summary, provenance |
| C4 procedural | procedure recalled, versioning keeps old and new |
| C5 knowledge-grounded | recall says where a memory came from |
| C6 personalized | provenance, changed preference, tenant scope, per-user private memory |
| C7 constraint-aware | viewer read-only, secrets not returned raw, deny policy enforced |
| C8 agentic | program as a memory object, exactly-once replay, lifecycle evolution |

C5's real evidence is model-in-the-loop: the Vedas Ask run (`docs/CHAT.md`).

## Results (2026-11-13, branch `fix/index-checkpoint-resume`, debug build)

Five runs: **27 checks pass 5/5, 2 are gaps 5/5, none flaky.**

Running the harness found five defects. All five are fixed on that branch,
each with a test that fails first:

| Found by | Defect | Fix |
|---|---|---|
| CHAT-MEM-005 | `agent_memory` stored a memory tenantless, so a tenant-scoped agent could not read back its own write; another tenant writing the same key got the first tenant's object back | the memory carries the caller's tenant |
| PROC-CHAT-001, FORGET-001 | the default `recall` (and `memory_search`) goes vector-only once the question embeds, so a memory whose vector had not landed yet was invisible (1 miss in 20 store-then-recall pairs) | a memory with no vector competes by its text rank |
| AUTH-001 | a `viewer` token could write and delete memory unless the enterprise licence was active | the viewer role is read-only in every mode: MCP, native protocol and REST writes |
| SENS-001 | `recall` returned a stored AWS key verbatim | `recall` and `memory_search` return `[REDACTED:<KIND>]` |
| (fix review) | `vector_led` mixed BM25 and cosine scores, and `memory_search`'s MMR divides by the top score | one score scale, `61/(61+rank)` |

The two gaps are product decisions still to make:

- **PERS-005, per-agent private memory over TCP.** Every TCP token is the
  agent `tcp-agent`, and `session_init` refuses an `agent_id` over TCP. Two
  agents in one tenant are therefore one principal, and a "private" memory is
  shared by all of them. The token spec (`TOKEN:TENANT:ROLES`) has no field
  for the agent's name.
- **POLICY-001, policies are advisory.** `evaluate_policies` answers
  "denied" for the real caller (`tcp-agent`), but the denied `forget` still
  succeeds. In the code, only the `evaluate_policies` tool reads policies; no
  write path consults them.

## Read the numbers with care

- **The semantic side of recall mostly did not run.** In the debug build the
  embedding model is busy with background enrichment, so `recall` falls back
  to text-only. The vector leg ran in one check (PROC-CHAT-001); the rest
  were keyword recall on questions that share words with the stored text.
  Paraphrased questions (no shared words) need a release build with the
  model idle, and are not yet checked.
- **Document memory has no vectors.** The document pipeline embeds chunks
  with a mock provider, and saving a document keeps none of those vectors.
  Search over an ingested document is therefore keyword-only.
- Almost every check runs as the tenant's admin token, so ownership and ACL
  paths are mostly untested. PERS-002 passes on any provenance text.
- Redaction covers API keys, bearer and JWT tokens, AWS and private keys,
  passwords, emails, card numbers, SSNs and connection strings. A profile
  memory holding an email address therefore comes back redacted from
  `recall`. `aikoql`, `get`, `find_experiences` and `agent_memory` still
  return stored values as they are.
- A pass means the store did what the check asks. It does not measure answer
  quality: that depends on the model reading the memory.
