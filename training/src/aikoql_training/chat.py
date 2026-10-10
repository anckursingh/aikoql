"""T-16 + T-20: the inference wrapper (design §40 end-state) — the
thin question -> query -> context -> answer path, the POC chatbot
core.

`chat` reuses the validated pipeline seam-for-seam: the two model
skills run through the T-20 JSON protocol (build_query_prompt /
build_answer_prompt / parse_query_reply / parse_answer_reply) around
one live AIKOQL call; no other retrieval exists in the path. The
model (`generate`) and the query runner (`run_query`) are injectable
so the wrapper is tested without a model — the POC chatbot is
scripts/chat.py.

Refusal paths, all machine-readable (T-09 + T-20):
- the model reply is not valid protocol JSON -> UNKNOWN: refused
  fail-closed, never a guessed answer;
- the model declares a refusal (refusal_reason field) -> carried
  through as a field;
- the query failed to compile or execute -> UNKNOWN: query failed;
- the query returned no results -> UNKNOWN: query returned no
  results;
- the model's answer reply refuses (UNKNOWN: + refusal_reason) ->
  passes through with grounded=false and no claims.
"""

from __future__ import annotations

from typing import Any, Callable, Dict, List

from aikoql_training.errors import ModelOutputError
from aikoql_training.inference import (
    build_answer_prompt,
    build_query_prompt,
    parse_answer_reply,
    parse_query_reply,
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
             retrieved: List[str], refusal_reason: str = None) -> Dict[str, Any]:
    return {"question": question, "query": query, "compiled": compiled,
            "retrieved": retrieved, "statements": [], "claims": [],
            "grounded": False, "refusal_reason": refusal_reason,
            "answer": unknown_answer(reason), "refused": True}


def chat(question: str, *, generate: Generate,
         run_query: RunQuery) -> Dict[str, Any]:
    """One question through the full path; the record carries the
    query, live compile/retrieval, the answer, its claims and the
    grounding verdict (refused=True whenever the answer is a refusal)."""
    try:
        reply = parse_query_reply(generate(build_query_prompt(question)))
    except ModelOutputError:
        return _refusal("model reply was not valid protocol JSON",
                        question, "", False, [])
    if reply["refusal_reason"] is not None:
        return _refusal(f"model refused: {reply['refusal_reason']}",
                        question, "", False, [], reply["refusal_reason"])
    query = reply["query"]
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
    try:
        answer = parse_answer_reply(
            generate(build_answer_prompt(question, statements)))
    except ModelOutputError:
        return _refusal("model answer was not valid protocol JSON",
                        question, query, True, retrieved)
    if answer["refusal_reason"] is not None:
        return {"question": question, "query": query, "compiled": True,
                "retrieved": retrieved, "statements": statements,
                "claims": [], "grounded": False,
                "refusal_reason": answer["refusal_reason"],
                "answer": answer["answer"], "refused": True}
    return {"question": question, "query": query, "compiled": True,
            "retrieved": retrieved, "statements": statements,
            "claims": answer["claims"], "grounded": answer["grounded"],
            "refusal_reason": None, "answer": answer["answer"],
            "refused": False}
