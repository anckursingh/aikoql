# Ask — grounded chat over your knowledge

`POST /api/v1/chat` and the Studio **Ask** panel answer a question from the
knowledge in your database, using any OpenAI-compatible model endpoint, and
**refuse** whenever the retrieved facts do not support the answer. Nothing
changes unless a `[chat]` table is configured; without it the route answers
503 and Studio says "No model configured".

## The flow

1. **Query skill.** The model gets the question and may reply with one AikoQL
   `MATCH` query (or decline). The query runs read-only, as the caller, confined
   to the selected tenant. A `CREATE`/`UPDATE`/`DELETE`/`INGEST` is never run.
2. **Fallback.** If the model declines, the query does not compile or returns
   nothing, the server retrieves with its own hybrid search over the same
   tenant. If that finds nothing too, the answer is a refusal and the model is
   not asked again.
3. **Answer skill.** The model gets the question and at most **8 facts /
   2000 characters** (each value capped at 200 characters; ingestion bookkeeping
   such as `confidence` and `evidence_*` is left out). A passage — a document
   page, chunk or fragment — is shown as at most 480 characters centred on the
   question's words, with its spacing collapsed, and a passage already shown
   for the same document is skipped, so several passages fit. Secrets are
   redacted (`[REDACTED:<KIND>]`) *before* the facts leave the server.
4. **Grounding check, by the server.** Every claim must quote a retrieved fact:
   either the whole fact, or a run of at least four of its whole words in their
   order (case, spacing and edge punctuation aside; no paraphrase, no stitching
   two runs together, no word cut in half). Otherwise the answer is
   downgraded to `UNKNOWN: answer not supported by the retrieved facts`.
   A quote is checked word for word, not for meaning: it can leave out a
   qualifier ("not", "only") that sits outside the quoted words, and the
   `answer` text itself is not matched. Treat a grounded answer as "these
   words appear in the source", not as a proof of the answer. The
   model's `grounded` flag and its `evidence_ids` are not trusted: it is shown
   statements, not koids, so the server attaches each claim's evidence from the
   fact it matched.
5. **Audit.** One line per call via the normal audit log (`tool: chat`):
   outcome (`answered`/`refused`/`error`), tenant, context source, model id,
   latency and the koids retrieved. The answer text is not stored.

The response record: `question, answer, grounded, refused, refusal_reason,
claims[{statement, evidence_ids}], query, context_source
("model_query"|"retrieval"|"none"), retrieved[koid], statements[], model, ms`.

## Configuration

```toml
[chat]
url = "http://127.0.0.1:8080"   # base URL, or a full .../chat/completions URL
model = "qwen2.5-0.5b-lora"
api_key_env = "CHAT_API_KEY"    # optional: name of the env var holding the bearer key
timeout_secs = 60               # optional, default 60
```

Unknown keys are rejected. A base URL gets `/v1/chat/completions` appended. If
`api_key_env` names a variable that is not set, calls fail with a clear error
instead of going out unauthenticated.

`GET /api/v1/chat/status` returns `{enabled, model, reachable}` (`reachable` is
a 2-second TCP probe of the endpoint).

## Who sees what

- A request needs a signed-in session. Viewers can ask (it is a read).
- The tenant comes from the request (`{"question", "tenant"}`; Studio's selector).
  Facts and model-written queries are confined to it. Without a tenant, only a
  caller whose roles can see across tenants (admin) sees tenant data.
- REST sessions carry no tenant claim of their own, so today a signed-in user can
  pick any tenant here exactly as they can for the ontology and graph routes.

## Serving a model

**The free model.** The training pipeline in `training/` fine-tunes
`Qwen/Qwen2.5-0.5B-Instruct` with a LoRA adapter on this exact protocol. Merge
the adapter, convert to GGUF and serve it with llama.cpp's `llama-server` or
Ollama, both of which expose an OpenAI-compatible endpoint; point `[chat] url`
at it.

Serving it through Ollama, two things matter:

- **Import with the base model's tokenizer and config.** A merged model saved
  by transformers 5.x imports cleanly but answers one token forever (`@@@@`).
  Copy `tokenizer.json`, `vocab.json`, `merges.txt`, `tokenizer_config.json`
  and `config.json` from the base model's snapshot into the merged directory
  before `ollama create`.
- **Pass the prompt through untemplated.** The adapter was trained on raw
  prompt → completion text, so the Modelfile needs `TEMPLATE "{{ .Prompt }}"`,
  `PARAMETER temperature 0` and stops on `<|endoftext|>` and `<|im_end|>`. The adapter/GGUF is a separate download and is not in this repo or the
binary. "Free" means no licence fee: you supply the compute. Check the base
model's licence (Qwen2.5-0.5B is published under Apache-2.0) before shipping it.

**Your own LLM.** Any OpenAI-compatible endpoint works. **The retrieved tenant
facts are sent to whatever endpoint you configure** — that is a data-handling
decision for your enterprise, not something the server can undo. The prompts are
fixed and must stay byte-identical to `training/src/aikoql_training/inference.py`
(pinned by `crates/services/api/mcp/tests/fixtures/chat/prompts.json`), because
the adapter was trained on them. The reply parsers are strict (one JSON object,
no unknown or duplicate keys, both `query` and `refusal_reason` present). A
general-purpose model that follows the prompt literally and omits
`refusal_reason` is treated as having declined, so retrieval supplies the facts.
That is safe, but such a model rarely gets to write the query.

## What to expect (known limits)

From the training scorecards (`training/artifacts/scorecards`, 24 synthetic
questions, base `Qwen2.5-0.5B-Instruct`):

| | no adapter | LoRA adapter |
|---|---|---|
| query compile rate | 0% | 45.8% (11/24) |
| KO recall | 0% | 37.5% (9/24) |
| KO precision | 0% | 100% (9/9) |
| groundedness | 33.3% | 33.3% |
| false refusals / secret leaks | 0 / 0 | 0 / 0 |

Unanswerable questions were not part of that set. Because of those numbers the
fallback and the server-side grounding check exist: expect a small model to be
refused often, which is the safe failure.

The retrieval fallback has **no relevance cutoff**: hybrid search always returns
its top hits, so an off-topic question still yields facts. Refusing an
unanswerable question is therefore the model's job, and a weak model will not
always do it. Measure before relying on it.

## Measuring a model

```sh
python3 scripts/chat-eval.py --url http://127.0.0.1:8098 --user admin \
  --password … --questions crates/services/api/mcp/tests/fixtures/chat-eval/questions.json
```

26 hand-written questions about `AGENTS.md`, `README.md` and `CHANGELOG.md`
(ingest them with `aikoql-mcp ingest-dir`), 21 answerable and 5 not. It prints
the training scorecard's metrics. `scripts/chat-stub.py` is a deterministic
stand-in model (it declines to write queries and quotes the first retrieved
fact); the committed `baseline.json` was produced with it and only proves the
harness: `unknown_recall` 0/5 (a quoting stub never refuses), `ko_recall` 5/21
(retrieval alone), no secret leaks.

**Real adapter:** not run — the adapter weights are not available on the
machine this was built on. Record its numbers here when you run it.
