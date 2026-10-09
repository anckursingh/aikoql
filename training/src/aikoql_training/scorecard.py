"""The model scorecard (design §32/33/34) — eight machine-checkable
metrics over a split of the corpus, computed from prediction records.

Each prediction is {example_id, query, answer} plus, when a live run
recorded them, `compiled` (the query's real compile result) and
`retrieved` (the koids its execution returned). The metrics:

- query_compile_rate — the `compiled` flag when present, else the E3
  static head check (MATCH/TRAVERSE + non-empty). A live run's compile
  verdict always wins.
- ko_recall / ko_precision — retrieved koids vs expected koids over
  the examples that carry koids, with the oracle's targets rule (a
  TRAVERSE never returns its source KO: multi-koid examples check
  koids[1:]).
- groundedness — the predicted answer re-run through
  validate_grounding against the example's gold context/labels: every
  non-refusal answer must trace to a context fact with its evidence
  (the T-07 substring ceiling).
- unknown_recall — refused / unknown: unknown examples answered with
  the UNKNOWN: prefix. None when the split has no unknown examples.
- unknown_precision — refused / (refused + false_refusals): of every
  UNKNOWN: answer, the share that landed on an unknown example. None
  when the model issued no UNKNOWN: answers at all (a model that
  never refuses scores None, never a fake 0.0).
- false_refusal_rate — false_refusals / answerable: UNKNOWN: answers
  on answerable examples. None when the split has no answerable
  examples. detail carries the denominators.
- secret_leak_rate — predictions whose query or answer matches the
  dataset secret patterns (the gates' _SECRET_PATTERNS, never a
  second list).

An example with no prediction record counts as failed everywhere
(missing = no query, no answer, no retrieval). Values are 0..1, or
None when the metric's denominator is absent from the split (§27:
an undefined rate must never read as 0.0); detail carries the counts.

T-26: the same eight metrics are also reported per capability —
by_task (task.type) and by_difficulty — so a failure concentrated in
one capability cannot hide behind the aggregate (PR9 Finding #5).
"""

from __future__ import annotations

from typing import Any, Dict, List, Optional, Tuple

from aikoql_training.dataset.gates import _SECRET_PATTERNS
from aikoql_training.scenarios.answer_formats import UNKNOWN_PREFIX
from aikoql_training.validation.eval_set import _HEAD
from aikoql_training.validation.grounding import validate_grounding


def _metric(value: Optional[float], **detail: int) -> Dict[str, Any]:
    return {"value": value, "detail": detail}


def _targets(koids: List[str]) -> List[str]:
    """The oracle's match rule (gates.py scenario_match): a TRAVERSE
    result never carries the source KO."""
    return koids[1:] if len(koids) > 1 else koids


def _compiled(pred: Dict[str, Any], query: str) -> bool:
    if "compiled" in pred and pred["compiled"] is not None:
        return bool(pred["compiled"])
    return bool(query.strip()) and bool(_HEAD.match(query))


def _cell(examples: List[dict], by_id: Dict[str, dict]) -> Dict[str, Any]:
    """The eight metrics over one example group (the whole split, or one
    capability cell). §27 holds per group: an absent denominator is None."""
    compiled_n = leaks = 0
    recall_hits = recall_targets = precision_retrieved = 0
    grounded_n = 0
    unknown_n = refused_n = false_refusals = 0
    missing = 0
    for ex in examples:
        eid = ex.get("example_id")
        pred = by_id.get(eid)
        if pred is None:
            pred = {}
            missing += 1
        query = str(pred.get("query") or "")
        answer = str(pred.get("answer") or "")
        task = (ex.get("task") or {}).get("type")

        if _compiled(pred, query):
            compiled_n += 1
        if any(p.search(query) or p.search(answer)
               for p in _SECRET_PATTERNS):
            leaks += 1

        koids = [str(k) for k in (ex.get("expected") or {}).get("koids") or []]
        if koids:
            targets = set(_targets(koids))
            retrieved = [str(k) for k in pred.get("retrieved") or []]
            hits = sum(1 for k in retrieved if k in targets)
            recall_targets += len(targets)
            recall_hits += hits
            precision_retrieved += len(retrieved)

        pseudo = {**ex,
                  "expected": {**ex.get("expected", {}), "answer": answer}}
        if validate_grounding(pseudo)["ok"]:
            grounded_n += 1

        if task == "unknown":
            unknown_n += 1
            if answer.startswith(UNKNOWN_PREFIX):
                refused_n += 1
        elif answer.startswith(UNKNOWN_PREFIX):
            false_refusals += 1

    total = len(examples)
    # explicit denominators: a rate whose denominator is absent from
    # the split is None (§27), never a silent 0.0
    unknown_recall = refused_n / unknown_n if unknown_n else None
    unknown_precision = (
        refused_n / (refused_n + false_refusals)
        if refused_n + false_refusals else None)
    false_refusal_rate = (
        false_refusals / (total - unknown_n)
        if total - unknown_n else None)
    return {
        "example_count": total,
        "missing_predictions": missing,
        "metrics": {
            "query_compile_rate": _metric(compiled_n / total if total else 0.0,
                                          compiled=compiled_n, total=total),
            "ko_recall": _metric(
                recall_hits / recall_targets if recall_targets else 0.0,
                hits=recall_hits, targets=recall_targets),
            "ko_precision": _metric(
                recall_hits / precision_retrieved if precision_retrieved
                else 0.0,
                hits=recall_hits, retrieved=precision_retrieved),
            "groundedness": _metric(grounded_n / total if total else 0.0,
                                    grounded=grounded_n, total=total),
            "unknown_recall": _metric(unknown_recall,
                                      refused=refused_n, unknown=unknown_n),
            "unknown_precision": _metric(unknown_precision,
                                         refused=refused_n,
                                         false_refusals=false_refusals),
            "false_refusal_rate": _metric(false_refusal_rate,
                                          false_refusals=false_refusals,
                                          answerable=total - unknown_n),
            "secret_leak_rate": _metric(leaks / total if total else 0.0,
                                        leaks=leaks, total=total),
        },
    }


def compute_scorecard(
    predictions: List[dict],
    ds: Dict[str, Any],
    *,
    split: str = "test",
) -> Dict[str, Any]:
    """The eight metrics over `split` plus a per-capability breakdown
    (T-26): by_task keyed on task.type, by_difficulty on
    task.difficulty — each cell the same shape as the aggregate, only
    present capabilities listed. Predictions are joined to examples by
    example_id (unmatched predictions are ignored)."""
    examples = ds[split]
    by_id = {p.get("example_id"): p for p in predictions if p.get("example_id")}
    out = _cell(examples, by_id)

    def _grouped(key: str) -> Dict[str, Dict[str, Any]]:
        groups: Dict[str, List[dict]] = {}
        for ex in examples:
            name = (ex.get("task") or {}).get(key)
            if isinstance(name, str) and name.strip():
                groups.setdefault(name, []).append(ex)
        return {name: _cell(g, by_id) for name, g in sorted(groups.items())}

    out["by_task"] = _grouped("type")
    out["by_difficulty"] = _grouped("difficulty")
    return out
