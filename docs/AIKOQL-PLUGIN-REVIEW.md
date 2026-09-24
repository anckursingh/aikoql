# AikoQL MCP Plugin — Review: Utility for Coding Agents

Date: 2026-09-09 · Scope: plugin v0.1.19 (`npx -y aikoql-mcp@0.1.19`, the released npm server) ·
Method: live stdio round-trip against the real KB (`C:\Users\ancku\aikoql-kb`) during a coding
session, plus code inspection of the MCP surface. Honest ledger: every finding below was
reproduced, not inferred.

## What it is

One MCP server exposing the whole knowledge-OS kernel — **90 tools** across knowledge CRUD,
aikoql queries, graph traversal, provenance, hybrid search, context compilation, document
ingestion, program deployment, constraint validation, and storage ops. Single embedded binary,
zero runtime deps, optional encryption at rest, one KB per user (the aikoql-v2 storage engine
underneath). For a Claude Code session it is a *persistent substrate* the conversation itself
never provides.

## Live round-trip (this session)

| Step | Result |
| --- | --- |
| `initialize` | OK — aikoql-mcp 0.1.19 |
| `tools/list` | 90 tools |
| `remember` (dev_log note about P3-M3) | OK — KO `01a086c9…a9c9`, version 1 |
| `aikoql` `MATCH dev_log RETURN *` | 1 row — the new KO, koid **MATCH** |
| `forget` × 2 (duplicate dev_logs from harness iterations) | OK |

## Strengths — mapped to coding-agent jobs

1. **Cross-session memory.** `remember`/`get`/`aikoql` + `memory_store`/`memory_search`/
   `agent_memory`: an agent resumes yesterday's debugging context, decisions, and findings
   from the KB instead of a transcript that scrolled away. This is the plugin's core value.
2. **Provenance and trust.** `trace`/`explain`/`prove`/`verify_knowledge`/`provenance`: an
   agent can cite *where* a fact came from. `contradict`/`find_conflicts`/`find_stale`/
   `resolve_conflict` keep the KB from rotting when several agents write into it.
3. **Context compilation.** `compile_context`/`document_ingest`/`summarize_conversation`:
   an agent builds a context packet from KB state instead of raw dumps — directly the
   context-window problem coding agents live with.
4. **Scriptable operations.** `backup`/`restore`/`list_backups`/`verify_backup`/`metrics`/
   `health`/`abi_version`: DB ops the agent can drive itself (the P3-M3 milestone makes this
   path engine-native for v2).
5. **The KB grows tools.** `deploy_program`/`execute_program`/`deploy_policy`/`deploy_workflow`/
   `deploy_trigger`/`deploy_connector`/`deploy_view`/`deploy_report`/`deploy_benchmark` +
   `aikoql` as the query language: custom surfaces without a redeploy.
6. **Governance for agent teams.** `session_init`/`decide`/`reason`/`infer`/`predict` for
   coordination; `audit_report`/`compliance_report`/`evidence_pack`/`filter_secrets` so an
   agent's outputs can be reviewed and redacted before they leave the machine.

## Friction — reproduced this session

1. **`remember` rejects the natural call shape.** `{content: "..."}` fails with
   `VALIDATION_ERROR: missing argument: type_name`; the text belongs in `note`. An LLM
   calling this tool will hit this on the first call of every session.
2. **`get` returns metadata only.** The KO comes back with `event_refs: 1` and no text —
   the note content lives in events, and no tool exposes event reads. An agent cannot
   cheaply re-read what it stored (had to verify via `aikoql MATCH` instead).
3. **`memory_search` breaks on CWD.** Defaults to `./memory` → `INTERNAL: cannot read memory
   dir './memory'`. The plugin launches from whatever CWD Claude Code happens to be in;
   relative paths resolve wrong. Needs `--memory-dir` — a flag an agent never passes.
4. **`aikoql` requires `subject`.** Identity must be threaded explicitly; a session default
   from `session_init` would remove the ceremony.
5. **Rate limit 120 calls/min** (default config) — a busy agent bursts past this quickly.
6. **No MCP resources** — discoverability is `tools/list` only.

## Verdict

Right architecture, small teeth. Embedded single binary, per-user KB, provenance-first
toolset, and a query language — that is the correct shape for an agent memory substrate.
The six frictions above are all surface-level; none touches the kernel. Top three gaps for
coding agents specifically: **content read-back (2)**, **remember ergonomics (1)**,
**memory_search path resolution (3)**.

## Recommendations (ranked)

1. Expose event text: `get` gains the latest note/event payload (or add `get_event`).
2. `remember` accepts `content` as an alias for `note` (keep `type_name` required — the
   type discipline is good).
3. Resolve `memory_dir` against the KB path (or `userConfig.KB_PATH`), never CWD.
4. Default `subject` to the session subject established by `session_init`.
5. MCP resources listing KOs per type (cheap discoverability win).
6. Per-agent rate-limit keys or a documented operator override for agent workloads.
