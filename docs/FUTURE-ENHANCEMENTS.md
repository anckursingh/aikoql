# Future enhancements and open items

Status: written 2026-11-12 at the end of the Studio / ontology / document-knowledge-graph work
(PRs #33, #34, #47). Everything here was found or decided while using the product on real data
(a 9 MB, 1,446-page PDF and ClickHouse + Postgres case study). Nothing in this file is implemented
unless it says so. Each item carries why it matters and what "done" means.

## 1. Model-assisted extraction (its own PR, evaluation first)

**Problem.** Document compile uses a rules-based extractor (`MockSemanticAnalyzer`, "rules-v1").
Storage and text retrieval of a saved document are exact, but the extracted *graph* is weak: on the
Vedas book many entities are text fragments ("WITH THE APAMARGA", "HARM AGAINST SCROFUL") and almost
every relation is a generic `related_to`.

**Decision (2026-11-12).** Do it in a separate PR, after the current PRs merge, **benchmark first,
production extractor last**. Do not implement the extractor before the benchmark can say whether it helps.

**What already exists.**
- `SemanticAnalyzer` (crates/ingestion/src/ir.rs) is a pluggable trait; its doc comment anticipates LLM
  implementations. `Evidence` already carries `extractor` and `model` for provenance.
- `crates/ingestion/tests/fixtures/extraction-bench` has hand-written entity gold and a ratchet
  (`results.json`), but only **26 dev + 21 holdout entities**: one entity is about 5 points of recall, so
  it cannot decide a model-vs-rules question. No relation, coreference, evidence or hallucination gold.
- `training/` fine-tunes *retrieval* models (question -> query -> grounded answer). It is not an
  extraction benchmark, but its style (deterministic, fail-closed scorecards, holdout discipline,
  refusal metrics) is the one to copy.
- docs/CHAT.md already documents serving a model over an OpenAI-compatible endpoint (Ollama included).

**Configurations to compare.**
| | what it is |
|---|---|
| A | rules only (today) |
| A' | rules plus the obvious rule fixes (all-caps heading fragments, stop-phrases). Without this arm the model is credited for fixing rule bugs |
| B | rules plus model enrichment: the model may only add or retype what rules found |
| C | model-assisted extraction with deterministic validation: the model proposes entities and relations with a **verbatim evidence quote**; a validator accepts an item only if the quote exists at the cited place in the source and the item fits the schema; chunks that fail fall back to rules |

Expected winner: **C**. Hallucination is bounded by construction (every accepted item has checkable
evidence), and rules stay the default and the fallback, so reliability is not compromised.

**Metrics.** Entity precision/recall (strict and lenient span overlap), relation precision/recall/F1
(typed and predicate-agnostic; rules only emit co-occurrence, so typed relations are a structural win,
the real questions are precision and hallucination), name-variant coreference (B-cubed; not pronouns, the
graph needs aliases), evidence accuracy (quote exists verbatim = deterministic; quote supports the claim =
judged), hallucination rate, extraction latency (chunks/second, total time per book: 1,584 chunks at 10 s
is about 4.4 h) and cost.

**Gold set.** About 150-200 chunks across the real document types (verse prose, letters and invoices,
technical docs, tables). Build it by **pooled judging**: run all arms, label the union of their outputs
correct/incorrect and add what was missed (cheaper than writing gold, favours no arm). Keep a split never
used for prompt tuning. Report paired comparisons with bootstrap confidence intervals.

**Harness shape.** Keep the model step separable: a CLI emits extractions as JSONL, a scorer reads JSONL
anywhere. That lets the model step run on a Windows laptop with Ollama while scoring runs elsewhere. Put the
scorer next to the training scorecards. Use Ollama's structured JSON output for reliable parsing. Cache by
(model digest, prompt version, chunk hash) so results are reproducible; record model name and digest in
`Evidence.model`.

**Ship gate.** Only if C beats A' with a confidence interval that excludes zero on precision and F1 and the
validated hallucination rate is about zero. Ship opt-in (`[extraction] enabled = false` by default), as a
background step after save, never in the save path's critical path. Treat document text as untrusted input to
the model (schema-validated output, no tools).

**Open inputs needed:** the laptop's GPU/VRAM and which models are pulled; willingness to judge about 1,500
items; acceptable time per book; whether the laptop is reachable from the dev machine or the harness runs on it.

## 2. Documents in the knowledge base

- **Cross-document entity resolution.** Compile passes `&[]` as the existing knowledge base, so "Indra" in two
  documents becomes two nodes. Feed existing entities to the resolver; propose merges, never merge silently.
- **Saving Markdown and Rust sources.** Refused today (their compilers produce `markdown_ir` / `code_ir`, a
  different shape). Either map them to the same objects or ingest as plain text.
- **Save is all-or-update.** Re-saving rewrites in place and prunes what vanished. Add: save a changed file as a
  new version of the same document, and a visible diff of what changed.
- **Compile time.** A 1,446-page book compiles in about 130 s; saving is about 195 s in total. Stream progress
  by phase, and consider caching the compile result by document hash so Save after Compile is instant.
- **Graph tab.** Expand a node's full neighbourhood on click, cluster by type or page, a legend for hidden
  edge labels in dense views, and relation filters. The graph is only as meaningful as the extractor (section 1).
- **Fragments for image pages.** A page the compiler left out of every chunk now gets a supplemental chunk;
  its fragment/AST representation is still missing. Fix at the source (the compiler), not only at save.
- **Per-property authority and contradiction review.** See section 3.

## 3. Ontology in practice (docs/ONTOLOGY-IN-PRACTICE.md)

- **Phase D, remaining:** edit a class (name, parent, properties) or a property map by hand; schema history
  (`AS_OF` over `aikoql:source-table`); the schema / sample / ingest mode choice; empty-source guidance
  (a source that discovers 0 containers should say "this database has no tables, check the name or credentials").
- **Phase E:** hydrated `TRAVERSE` and `SIMILAR TO` results (they return only `koid` / `rel_type` / `depth`,
  or `koid` / `score`); `JOIN` that resolves ontology classes (today the right side must be a physical type);
  incremental import by an `updated_at` watermark; canonical property names in result rows; a documented
  "similar devices" recipe (fingerprint components + embeddings; scoring is lexical until a real model is set).
- **Phase F:** `docs/CASE-DEVICE-INTELLIGENCE.md` (the story, the schema, the four questions with answers).
- **Phase G:** provenance and bitemporal metadata on imported objects (source object id, source updated-at,
  ingestion run, connector version, observed-at).
- **Authority:** a ranked source list per canonical property (today the rank is per assertion, so one container
  cannot rank differently for different properties); persist and edit authority in the Ontology panel.
- **Contradictions:** a review queue (accept / override with a reason), not only the Reconcile card.

## 4. Studio (docs/STUDIO-ENHANCEMENTS.md)

First-run guidance and actionable empty states; honest paging for the chunked query view; KOID links between
Inspector, Graph and Explorer; P2 structural work (split `app.js` into modules, escaped-template helper,
virtualised tables for more than 5k rows, visual-regression screenshots in the Chrome e2e).

## 5. Query language

- The end-of-statement rule (a clause after `RETURN` is an error) fixed a silent filter drop. Audit the rest of the
  grammar for the same class: unknown or duplicate clauses, brace-style `CREATE T {..}` (now an error that names
  the grammar `CREATE T a == 1, b == 2`), and error messages that point at the fix.
- Add a "did you mean" hint for the common SQL/Cypher habits (`LIMIT` after `RETURN`, `WHERE` after `RETURN`).

## 6. Operations and platform

- **Logging.** About 190 raw `eprintln!` sites in the server and 26 in the ingestion crate still reach stderr
  only; move them to the logger. On Windows, stderr is not captured into the log file.
- **Dependabot.** GitHub reports 7 vulnerabilities on the default branch (1 critical, 3 high, 3 moderate) as of
  2026-11-12. Triage them (the `rusqlite` / `libsqlite3-sys` advisory RUSTSEC-2026-0459 is already fixed in #33).
- **Windows.** Run the open items on Windows (the replication cross-host case, the Windows exe). CI's Windows job
  cannot check out a tree containing a path with a colon; keep such files out of the repository.
- **Flaky tests.** `tcp_rate_limit_is_shared_across_connections`, `community_mode_never_rotates`,
  `live_role_revocation` and the `admin_api` pair fail under load with an empty `health:` (server start-up timing)
  and pass alone. Make the harness wait for readiness instead of a fixed budget.
- **CI-only suites.** The SDK contract tests (Python, Go, TypeScript, Java, Rust) and the 5-language fuzz smoke run
  only in CI; a parser or protocol change must be checked against them before merging.

## 7. Process lessons (keep)

- Stage explicit paths; never `git add -A` in a working tree that holds the owner's untracked files (two such
  files were committed by mistake and had to be removed in a follow-up).
- Run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` before every
  commit, not only before a push.
- Run the Studio e2e to completion in the background (it takes several minutes); never cap it with a short timeout.
- A verifier that reads the result back and compares it with the plan found a real gap (an OCR'd page in no chunk)
  that every unit test had missed. Prefer a verifier over a count.
