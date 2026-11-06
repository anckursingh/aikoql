# Tenant chat ("Ask") — design

Status: draft for approval (2026-11-06). Nothing is built.

## Intent

An enterprise should be able to put a support chat bot on top of its own
knowledge base without building one. The bot answers only from that
tenant's AIKOQL knowledge (ingested documents, facts, ontology), cites
what it used, and says "I don't know" when the knowledge base does not
contain the answer. The model does the language work only (turn a question
into an AikoQL query, phrase an answer from retrieved facts). AIKOQL
supplies every fact. The long-term product is a ready-made bot per tenant.

The owner's constraints (their words, 2026-11-06):
- First: an Ask panel in Studio, tested by the owner on ingested data and on
  this repo (dogfood).
- Free for enterprises by default: a pre-trained model ships as an *optional*
  package of AIKOQL.
- Enterprises may bring their own LLM if they choose to pay for one.
- Laptop-agnostic: the owner trained the model on a different machine.
- Per-tenant training is for the model's language task, never for knowledge.

## Goals

1. `POST /api/v1/chat` and a Studio "Ask" panel: question in, answer out with
   the query used, the retrieved facts, claims with evidence IDs, and a
   refused/grounded verdict.
2. The model sits behind one OpenAI-style HTTP chat endpoint
   (`/v1/chat/completions`) configured in `aikoql.toml` `[chat]`: `url`,
   `model`, optional `api_key_env`, `timeout_secs`. Local llama.cpp/Ollama,
   a remote server and a hosted API are all the same setting.
3. Tenant isolation: the query runs as the signed-in subject, confined to
   their tenant exactly like every other Studio call. A chat can never read
   another tenant's objects.
4. Fail closed: bad model output, failed query, empty results, model
   unreachable, ungrounded claims all produce a clear refusal, never a guess.
5. Works without the owner's trained model: with any capable instruction
   model it still functions (same prompts), possibly with lower quality.
6. Every question and answer is audited (who, tenant, question, query,
   koids retrieved, verdict, model id, latency).

## Non-goals

- No model weights in the AIKOQL binary or in this repo; no training or
  fine-tuning inside the server.
- No embeddable widget, public/anonymous access, or end-user (customer)
  auth in this version. Studio users only.
- No chat memory across turns in v1 (each question is independent).
- No tool-calling agent loop; the flow is fixed: query, retrieve, answer.
- No claim of accuracy beyond what is measured (see Evaluation).

## Anti-goals (a failure even if it "works")

- An answer containing a statement that is not in the retrieved facts.
- Any cross-tenant read, or the model endpoint receiving data from a tenant
  other than the asker's.
- A silently invented answer when the model is down or confused.
- Hard dependency on a model for the rest of Studio or the server: with no
  `[chat]` configured everything else behaves exactly as today.

## The flow (ported from `training/src/aikoql_training/chat.py`)

1. **Query skill.** Prompt = the training prompt, byte-identical
   (`build_query_prompt`): the question plus the JSON-object instruction.
   Expected reply `{"query": "<aikoql>"|null, "refusal_reason": ...}`, exactly
   one set, strict parse (first JSON object, no unknown fields, no duplicate
   keys) as in `inference.py`.
2. **Run the query** as the subject through the existing `aikoql` query path
   (compile + execute). Results carry `koid` and a `statement`/properties.
3. **Fallback (owner-approved default):** if the model query is a refusal,
   is invalid, fails to compile, or returns no results, retrieve with the
   server's own retrieval (`find_similar` / text search over the tenant's
   objects and document facts), top-k bounded. The response says which path
   produced the context (`context_source: "model_query" | "retrieval"`).
4. **Answer skill.** Prompt = `build_answer_prompt(question, statements)`,
   byte-identical. Expected reply `{"answer", "grounded", "claims":
   [{"statement","evidence_ids"}], "refusal_reason"}`.
5. **Verify grounding server-side** (do not trust `grounded`): every claim's
   `statement` must match a retrieved statement and every `evidence_id` must
   be a retrieved koid/evidence id. Any violation downgrades the reply to a
   refusal ("answer not supported by the retrieved facts"). This is the
   server's own check; the training repo's `validation/grounding.py` is the
   reference.
6. Return the record: `question, answer, grounded, refused, refusal_reason,
   claims, query, context_source, retrieved[], statements[], model, ms`.

## Where it lives (builder's call within these bounds)

- Orchestration in Rust inside `aikoql-mcp` (new module beside
  `api_rest.rs`/`tools/`), using the already-present `ureq` for the model call
  (blocking, with the configured timeout; run off the request thread if the
  server's threading model needs it). No new heavy dependency.
- Prompt builders and strict reply parsers ported from
  `training/src/aikoql_training/inference.py` with the Python tests ported
  as the RED tests, plus a cross-check test that the Rust prompts equal the
  Python prompts (fixture strings) so the two cannot drift: a fine-tuned
  adapter is only valid for the exact training prompts.
- Config: `[chat]` table in `aikoql.toml` with `deny_unknown_fields`,
  same pattern as `[ingestion]`. Unset = chat disabled.
- REST: `POST /api/v1/chat` (session/cookie + CSRF rules as every write-like
  POST), `GET /api/v1/chat/status` returning `{enabled, model, reachable}`.
- Studio: an "Ask" panel (new sidebar item), tenant selector reusing the
  existing one, question box, answer card with a refused/grounded badge,
  collapsible "how I answered" (query, context source, facts with koids that
  link to the Inspector), and a clear "No model configured — see docs" state.
  Same Studio rules: delegated handlers, CSP `script-src 'self'`, all dynamic
  values through `esc()`.

## Packaging the free model (optional, outside the binary)

- Docs + a `scripts/` helper describing how to serve the adapter locally:
  base `Qwen/Qwen2.5-0.5B-Instruct` + the owner's LoRA adapter, merged and
  served by llama.cpp or Ollama as an OpenAI-compatible endpoint; then set
  `[chat] url`. The adapter/GGUF is a separate download (location is the
  owner's to publish; not in this repo).
- "Free" means no licence fee for the model; the enterprise supplies its own
  compute. The model licence (Qwen2.5-0.5B is Apache-2.0 as far as the
  builder should verify) is stated in the docs.
- Bring-your-own LLM: any OpenAI-compatible endpoint; the docs warn that
  quality of the JSON protocol depends on the model and that tenant facts are
  sent to whatever endpoint is configured (the enterprise's data-handling
  decision).

## Per-tenant adaptation (later, not v1)

The training engine can generate training data from a tenant's *schema and
ontology* (data shape, no facts) and produce a small per-tenant adapter
offline. v1 only needs the `[chat]` endpoint to be selectable per tenant
later; v1 uses one global `[chat]`. A per-tenant `[chat.tenants.<id>]` override
is a likely follow-up, not built now.

## Evaluation (how we know it is any good)

- Reuse the training repo's scorecard metrics on a *dogfood* question set
  written by hand against this repo's ingested knowledge: query compile rate,
  ko recall/precision, groundedness, false-refusal rate, unknown recall,
  secret-leak rate. Commit the question set and the baseline numbers
  (honest, including a no-model/stub run) like the extraction benchmark.
- Test with a stub model (deterministic fake `/v1/chat/completions` server in
  the tests) for the whole flow: happy path, refusal, malformed JSON,
  duplicate keys, ungrounded claim, failed query then retrieval fallback,
  empty results, model timeout/unreachable, cross-tenant isolation.
- Fuzz the reply parsers (seeded) as the repo already does.
- Studio e2e: Ask panel against the stub server (no model required in CI).
- Dogfood: the owner's trained adapter on this repo's knowledge, measured and
  reported, whatever the result.

## Known risks (stated up front)

- The committed scorecard for the trained adapter is 46% query compile,
  37% retrieval recall, 33% groundedness on 24 synthetic questions, and it
  was trained on synthetic AcmePay data, not ingested documents. The
  retrieval fallback and the server-side grounding check exist because of
  this; expect the first dogfood numbers to be modest.
- The adapter weights are not in this repo and the path in the scorecard is
  another machine; the dogfood run needs them copied or served.
- Small-model JSON compliance: parsers are strict, so a non-compliant model
  yields refusals, not wrong answers.

## Left to the builder

Module layout, threading, retrieval details for the fallback (which existing
search to reuse), top-k, prompt size caps, audit record shape, panel styling,
exact config names beyond those above, and the question-set contents.

## Decisions from the builder review (2026-11-06)

Answered by the repo:
- **Tenant scoping of model-written queries.** The query path enforces it at
  compile time: `compile_scoped(source, subject, roles, tenant)` puts the
  caller's roles and tenant into every Scan (`tools/query.rs`
  `execute_readonly`). Chat calls `execute_readonly` directly, never
  `tool_aikoql` (which also runs `CREATE`), so a model-written query is
  read-only by construction and tenant-confined by the same mechanism as
  every Studio query. A test must prove a model query naming another tenant's
  type/koid returns nothing, and that a model-written `CREATE`/`UPDATE`/
  `DELETE` is refused.
- **Prompts.** Only the question (query skill) and the question plus
  retrieved statements (answer skill) — no ontology in the prompt
  (`inference.py`). The Python file is the single source of truth; the Rust
  fixtures are copied from it.
- **Tenant selection.** The tenant is the session subject's own tenant claim
  (a tenant-scoped user is confined to it); an unscoped admin may pick a
  tenant, as in the existing Studio tenant selector. The model endpoint is
  called only after that check passes.
- **Audit sink.** The existing `audit::audit_log(db_path, agent_id, tool,
  outcome, detail)`, tool name `chat`; detail = tenant, koids retrieved,
  verdict, model id, latency, context source. The answer text is not stored
  (ids only), to keep chat content out of the audit trail. An audit-write
  problem is logged and does not block the answer (the existing function
  returns nothing).

My calls (builder may refine, owner may override):
- **Grounding match.** A claim's `statement` must equal a retrieved statement
  after trimming and case/whitespace normalisation (the prompt asks the model
  to quote context statements, so claims are quotes). No fuzzy matching:
  paraphrase in a claim is a refusal. The free-text `answer` is not matched.
  Evidence is attributed by the server: the model is shown statements, not
  koids, so its `evidence_ids` are ignored and each verified claim carries the
  koid(s) of the retrieved fact(s) it matched (owner decision 2026-11-06). If the training repo's `grounding.py` disagrees,
  the server's check wins and the difference is documented. Expect the
  small model to be refused often at first; that is the safe failure.
- **Statement of an object without a `statement` field.** Render
  `"<type> <label>: key=value; ..."` from its properties (bounded length), and
  for document chunks the chunk text. Same renderer for both retrieval paths.
- **Secrets.** Retrieved statements pass through the existing secret filter
  before being sent to the model endpoint.
- **Milestone.** "Done" means the stub-driven flow, parsers, Studio panel and
  tests are complete and green. The real-adapter dogfood run is a separate
  reported gate that depends on the owner supplying the weights; its numbers
  are reported, not required to pass.
- **Threading.** The model call is blocking with the configured timeout; the
  builder checks the server's request threading and moves it off the shared
  path if a slow model could starve other requests.
