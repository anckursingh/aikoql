"""The eval set (design §33): E1–E9, machine-checkable dataset checks.

Each case is a dataset-level consistency property over the corpus
artifact — the same properties the T-15 scorecard measures on the
model, made checkable here without one. `eval_dataset` reports
`{E1..E9: {ok, count, detail}}` over every split; ok iff zero
violating examples. The CLI's eval command exits 0 iff every case
is ok — a corpus whose own checks fail is not publishable.

E1 factual -> exact fact: the expected answer is stated verbatim by a
  context fact (substring of a fact statement).
E2 retrieval -> correct KO set: every context fact's entities are all
  present in the context's entity mentions.
E3 query -> compilable aikoql: the query_target.query is non-empty
  and opens with the grammar's MATCH/TRAVERSE head.
E4 multi-hop -> correct graph path: a multi-hop example carries at
  least three koids (anchor + intermediate + target).
E5 temporal -> correct version: the query carries AS_OF and the
  question names a month.
E6 provenance -> correct evidence: every expected.evidence_ids entry
  is the canonical identity of a context evidence row.
E7 unknown -> refusal: unknown examples are answerable=False, every
  other example answerable=True.
E8 authorization -> no sensitive context: policy.authorization_required
  is True, and a DENIED example's context facts are all policy-decision
  facts (a leaked object fact fails the case).
E9 contradiction -> conflict-aware: labels.contradictory is True and
  the answer is in the CONTRADICTED machine-readable shape.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List

from aikoql_training.validation.grounding import evidence_id

_CASES = tuple(f"E{i}" for i in range(1, 10))
_SPLITS = ("train", "val", "test")

_MONTHS = ("January", "February", "March", "April", "May", "June", "July",
           "August", "September", "October", "November", "December")
_HEAD = re.compile(r"^\s*(?:MATCH|TRAVERSE)\b")
_CONTRADICTED = re.compile(
    r"^CONTRADICTED: .*? \(claim [0-9a-f]{8}\) vs .*? "
    r"\(claim [0-9a-f]{8}\); conflict [0-9a-f]{8}, resolution .*$"
)
_DECISION = "Policy decision: "


def _examples(ds: Dict[str, Any]) -> List[dict]:
    return [e for n in _SPLITS for e in ds[n]]


def _out(ok: bool, count: int, detail: List[str]) -> Dict[str, Any]:
    return {"ok": ok, "count": count, "detail": detail}


def eval_dataset(ds: Dict[str, Any]) -> Dict[str, Dict[str, Any]]:
    """Run the nine checks over every example; ok iff zero violations."""
    bad: Dict[str, List[str]] = {c: [] for c in _CASES}
    for e in _examples(ds):
        tid = e.get("example_id", "?")
        task = (e.get("task") or {}).get("type")
        ctx = e.get("context") or {}
        facts = ctx.get("facts") or []
        statements = [f.get("statement") if isinstance(f, dict) else None
                      for f in facts]
        mentions = {m for ent in (ctx.get("entities") or [])
                    if isinstance(ent, dict)
                    for m in (ent.get("mentions") or [])}
        expected = e.get("expected") or {}
        labels = e.get("labels") or {}
        query = (e.get("query_target") or {}).get("query") or ""

        if task == "factual":
            answer = expected.get("answer")
            if not any(isinstance(s, str) and answer is not None
                       and answer in s for s in statements):
                bad["E1"].append(tid)
        for fact in facts:
            if not isinstance(fact, dict):
                continue
            for ent in fact.get("entities") or []:
                if ent not in mentions:
                    bad["E2"].append(tid)
                    break
        if not _HEAD.match(query):
            bad["E3"].append(tid)
        if task == "multi_hop" and len(expected.get("koids") or []) < 3:
            bad["E4"].append(tid)
        if task == "temporal":
            if "AS_OF" not in query:
                bad["E5"].append(tid)
            elif not any(m in str(e["input"].get("question", ""))
                         for m in _MONTHS):
                bad["E5"].append(tid)
        if task == "provenance":
            have = {evidence_id(ev) for ev in (ctx.get("evidence") or [])
                    if isinstance(ev, dict)}
            if any(i not in have for i in (expected.get("evidence_ids")
                                           or [])):
                bad["E6"].append(tid)
        if task == "unknown":
            if labels.get("answerable") is not False:
                bad["E7"].append(tid)
        elif labels.get("answerable") is not True:
            bad["E7"].append(tid)
        if task == "authorization":
            if (e.get("policy") or {}).get("authorization_required") is not True:
                bad["E8"].append(tid)
            elif str(expected.get("answer", "")).startswith("DENIED:"):
                if any(not (isinstance(s, str) and s.startswith(_DECISION))
                       for s in statements):
                    bad["E8"].append(tid)
        if task == "contradiction":
            if labels.get("contradictory") is not True:
                bad["E9"].append(tid)
            elif not _CONTRADICTED.match(str(expected.get("answer", ""))):
                bad["E9"].append(tid)

    return {c: _out(not bad[c], len(bad[c]), bad[c]) for c in _CASES}
