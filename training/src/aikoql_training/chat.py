"""T-16: the inference wrapper (design §40 end-state) — the thin
question -> query -> context -> answer path, the POC chatbot core.

`chat` reuses the validated pipeline seam-for-seam: the two model
skills run through the §40 prompt/parse contract (build_query_prompt /
build_answer_prompt / parse_model_reply) around one live AIKOQL call;
no other retrieval exists in the path. The model (`generate`) and the
query runner (`run_query`) are injectable so the wrapper is tested
without a model — the POC chatbot is scripts/chat.py.

Refusal paths, all machine-readable (T-09):
- the model produced no query -> UNKNOWN: no query produced;
- the query failed to compile or execute -> UNKNOWN: query failed;
- the query returned no results -> UNKNOWN: query returned no results;
- the model itself refuses (UNKNOWN: answer) -> passes through.
"""

from __future__ import annotations

from typing import Any, Callable, Dict, List

from aikoql_training.inference import (
    build_answer_prompt,
    build_query_prompt,
    parse_model_reply,
)
from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX, \
    unknown_answer

Generate = Callable[[str], str]
RunQuery = Callable[[str], Dict[str, Any]]


def _statements(results: List[Dict[str, Any]]) -> List[str]:
    """Context statements from the query results (the statement field,
    else the koid — the corpus fact shape carries both)."""
    return [str(r.get("statement") or r.get("koid") or "") for r in results]


def _refusal(reason: str, question: str, query: str, compiled: bool,
             retrieved: List[str]) -> Dict[str, Any]:
    return {"question": question, "query": query, "compiled": compiled,
            "retrieved": retrieved, "statements": [],
            "answer": unknown_answer(reason), "refused": True}


def chat(question: str, *, generate: Generate,
         run_query: RunQuery) -> Dict[str, Any]:
    """One question through the full path; the record carries the
    query, live compile/retrieval and the final answer (refused=True
    whenever the answer is a refusal)."""
    query, _ = parse_model_reply(generate(build_query_prompt(question)))
    if not query:
        return _refusal("no query produced", question, "", False, [])
    try:
        env = run_query(query)
    except Exception:
        return _refusal("query failed to compile or execute", question,
                        query, False, [])
    results = env.get("results") or []
    retrieved = [r.get("koid") for r in results if r.get("koid")]
    if not results:
        return _refusal("query returned no results", question, query,
                        True, retrieved)
    statements = _statements(results)
    a_reply = generate(build_answer_prompt(question, statements))
    _, answer = parse_model_reply(a_reply)
    if not answer:
        # ponytail: same fallback as finetune predict — a bare reply
        # with no markers is the whole answer
        answer = a_reply.strip()
    return {"question": question, "query": query, "compiled": True,
            "retrieved": retrieved, "statements": statements,
            "answer": answer, "refused": answer.startswith(UNKNOWN_PREFIX)}
