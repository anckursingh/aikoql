"""The dataset validator + gates (design §26).

validate_dataset enforces every §5 gate fail-closed; a poisoned
dataset passes only when the gate that should catch it is missing.
Static gates run on the artifact alone (schema, grounding,
authorization, secrets, leakage, duplicates); the live gates
(compiler, execution, scenario_match) run only with a db and report
"skipped" otherwise; determinism compares against a reference dataset
directory. The leakage gate recomputes the split assignment from the
manifest seed — recorded placement must agree AND the cross-holdout
koid pair count must be zero.

`publishable` is True only when every evaluated gate passes; skipped
and disabled gates never veto.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List, Optional

from aikoql_training.dataset.config import load_config
from aikoql_training.dataset.splitter import assign_splits
from aikoql_training.dataset.writer import read_dataset
from aikoql_training.errors import DatasetError, TrainingDataError
from aikoql_training.models import validate as validate_schema
from aikoql_training.validation.grounding import validate_grounding

_SPLITS = ("train", "val", "test")

# ponytail: local pattern set is the static ceiling — the ingestion
# secret-filter (Rust, S1-S8) binds at corpus time (T-14), never bypassed.
_SECRET_PATTERNS = [
    re.compile(r"sk-(?:live|test)-[0-9A-Za-z]{16,}"),
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    re.compile(r"xox[bap]-[0-9A-Za-z-]{10,}"),
    re.compile(r"ghp_[0-9A-Za-z]{20,}"),
    re.compile(r"Bearer eyJ[A-Za-z0-9_-]{20,}"),
]


def _gate(ok: bool, status: str, count: int = 0, detail: str = "") -> dict:
    return {"ok": ok, "status": status, "count": count, "detail": detail}


_REF = re.compile(r"'[^']*'|\"[^\"]*\"")


def _question_norm(q: str) -> str:
    """The question with quoted refs masked: template corpora share the
    normalized shape of every question STRUCTURALLY, so normalized
    overlap is diagnostic (PR9's own caveat), never a hard tooth."""
    return _REF.sub("<REF>", q).lower()


def _leakage_dimensions(splits: Dict[str, List[dict]],
                        examples: List[dict]) -> Dict[str, int]:
    """Cross-split overlap per dimension (PR9 Finding #4).

    HARD: `canonical` — the exact question text shared across splits;
    the model saw the answer in one split, the other can no longer
    measure it. Diagnostic: `identifier` (entity names), `normalized`
    (question templates), `answer`, `relation_pattern` — template
    corpora share these structurally ("Payments Team" owns many
    services), so they are REPORTED on the gate, never vetoed.
    """
    dims = {k: 0 for k in ("canonical", "identifier", "normalized",
                           "answer", "relation_pattern")}
    homes = {e["example_id"]: n for n in _SPLITS for e in splits[n]}
    seen: Dict[tuple, tuple] = {}  # attr -> (example_id, home)
    for ex in examples:
        home = homes[ex["example_id"]]
        attrs: List[tuple] = []
        q = ex.get("input", {}).get("question", "")
        if q:
            attrs += [("canonical", q), ("normalized", _question_norm(q))]
        answer = ex.get("expected", {}).get("answer")
        if answer is not None:
            attrs.append(("answer", str(answer)))
        for ent in ex.get("context", {}).get("entities", []):
            if isinstance(ent, dict) and ent.get("name"):
                attrs.append(("identifier", ent["name"]))
        rels = tuple(sorted(
            step["relation"]
            for step in ex.get("semantic_target", {}).get("plan", {})
                .get("steps", [])
            if step.get("op") == "traverse" and step.get("relation")))
        if rels:
            attrs.append(("relation_pattern", rels))
        for dim, value in attrs:
            key = (dim, value)
            owner = seen.setdefault(key, (ex["example_id"], home))
            if owner[1] != home:
                dims[dim] += 1
    return dims


def _secret_hits(examples: List[dict]) -> int:
    hits = 0
    for ex in examples:
        texts = [ex.get("input", {}).get("question", ""),
                 ex.get("expected", {}).get("answer", "")]
        texts += [f.get("statement", "") for f in
                  ex.get("context", {}).get("facts", [])]
        for t in texts:
            if isinstance(t, str) and any(p.search(t) for p in _SECRET_PATTERNS):
                hits += 1
    return hits


def validate_dataset(
    dataset: str,
    *,
    db: Optional[str] = None,
    token: Optional[str] = None,
    config_path: Optional[str] = None,
    reference: Optional[str] = None,
) -> Dict[str, Any]:
    """Run every §5 gate over the dataset at `dataset` (a directory)."""
    if db is not None and token is None:
        raise DatasetError("a db requires a token")
    config = load_config(config_path)
    try:
        got = read_dataset(dataset, verify=False)
    except DatasetError as e:
        # an unreadable dataset cannot be gated — but the validator must
        # still emit a report (fail-closed), never die without one
        return {"publishable": False,
                "gates": {"integrity": _gate(False, "fail", 1, str(e))},
                "example_count": 0}
    examples: List[dict] = [e for n in _SPLITS for e in got[n]]
    manifest = got["manifest"]
    gates: Dict[str, dict] = {}

    # -- integrity --------------------------------------------------------------
    # the manifest sha256 check, as a gate instead of a raise: a tampered
    # split must not stop the other gates (a planted secret must still be
    # caught by secret_scan and reported).
    from aikoql_training.dataset.writer import tampered_splits
    bad = tampered_splits(dataset, manifest)
    gates["integrity"] = _gate(not bad, "pass" if not bad else "fail",
                               len(bad), f"tampered: {bad[0]}" if bad else "")

    # -- schema --------------------------------------------------------------
    schema_errors = 0
    for ex in examples:
        try:
            validate_schema(ex)
        except TrainingDataError:  # SchemaError and DatasetError both
            schema_errors += 1
    gates["schema"] = _gate(schema_errors == 0, "pass" if schema_errors == 0
                            else "fail", schema_errors)

    # -- grounding + evidence coverage (one pass, two counts) ----------------
    grounding_errors = evidence_errors = 0
    grounding_detail = ""
    for ex in examples:
        out = validate_grounding(ex)
        if not out["ok"]:
            grounding_errors += 1
            if not grounding_detail and out["errors"]:
                grounding_detail = out["errors"][0]
            evidence_errors += sum(1 for e in out["errors"]
                                   if "evidence" in e)
    gates["grounding"] = _gate(grounding_errors == 0,
                               "pass" if grounding_errors == 0 else "fail",
                               grounding_errors, grounding_detail)
    gates["evidence"] = _gate(evidence_errors == 0,
                              "pass" if evidence_errors == 0 else "fail",
                              evidence_errors)

    # -- authorization ---------------------------------------------------------
    flag_mismatches = 0
    for ex in examples:
        is_auth = ex.get("task", {}).get("type") == "authorization"
        required = ex.get("policy", {}).get("authorization_required", False)
        if is_auth != required:
            flag_mismatches += 1
    gates["authorization"] = _gate(flag_mismatches == 0,
                                   "pass" if flag_mismatches == 0 else "fail",
                                   flag_mismatches)

    # -- secret scan ------------------------------------------------------------
    if config["secret_scan"]:
        hits = _secret_hits(examples)
        gates["secret_scan"] = _gate(hits == 0,
                                     "pass" if hits == 0 else "fail", hits)
    else:
        gates["secret_scan"] = _gate(True, "disabled")

    # -- leakage: assignment must agree, be violation-free AND keep the -
    #    canonical question out of two splits (T-19: PR9 Finding #4) --------
    leakage_detail = ""
    try:
        held_out = tuple(manifest.get("held_out_orgs") or ())
        splits, violations = assign_splits(examples, manifest["seed"],
                                           config["ratios"],
                                           held_out_orgs=held_out)
        misplaced = 0
        for name in _SPLITS:
            home_ids = {e["example_id"] for e in splits[name]}
            misplaced += sum(1 for e in got[name]
                             if e["example_id"] not in home_ids)
        dims = _leakage_dimensions(splits, examples)
        ok = misplaced == 0 and not violations and dims["canonical"] == 0
        if misplaced:
            leakage_detail = f"{misplaced} example(s) misplaced"
        elif violations:
            leakage_detail = f"{len(violations)} cross-holdout koid pair(s)"
        elif dims["canonical"]:
            leakage_detail = (f"{dims['canonical']} canonical question(s) "
                              "cross the holdout")
        gates["leakage"] = _gate(ok, "pass" if ok else "fail",
                                 misplaced + len(violations)
                                 + dims["canonical"], leakage_detail)
        gates["leakage"]["dimensions"] = dims
    except DatasetError as e:
        gates["leakage"] = _gate(False, "fail", 1, str(e))

    # -- duplicates ------------------------------------------------------------
    ids = [e["example_id"] for e in examples]
    dup_count = len(ids) - len(set(ids))
    bound = config["max_duplicate_rate"] * max(len(ids), 1)
    gates["duplicates"] = _gate(dup_count <= bound,
                                "pass" if dup_count <= bound else "fail",
                                dup_count)

    # -- live gates -------------------------------------------------------------
    for name in ("compiler", "execution", "scenario_match"):
        gates[name] = _gate(True, "skipped")
    if db is not None:
        import aikoql
        # generous connect timeout: validation runs right after seeding,
        # when the server is busy with Tantivy indexer commits — the
        # default 5s socket timeout flakes there (initialize has no
        # per-call deadline and re-raises the raw socket timeout).
        with aikoql.Agent.connect(db, token=token, timeout=60.0) as agent:
            compile_errors = exec_errors = match_errors = 0
            match_detail = ""
            for ex in examples:
                q = ex.get("query_target", {}).get("query", "")
                if not q:
                    continue
                try:
                    env = agent.aikoql(q)
                except Exception:
                    compile_errors += 1
                    exec_errors += 1
                    match_errors += 1
                    continue
                if "results" not in env:
                    exec_errors += 1
                    match_errors += 1
                    continue
                rows = {r.get("koid") for r in env["results"]}
                koids = ex.get("expected", {}).get("koids", [])
                # The oracle's match rule: every hop TARGET must be
                # recovered. A TRAVERSE result never carries the source
                # KO (runtime RowSet::Traversal), so multi-koid examples
                # check koids[1:] — the single-koid (anchored MATCH)
                # shapes check the koid itself.
                targets = koids[1:] if len(koids) > 1 else koids
                missing = [k for k in targets if k not in rows]
                if missing:
                    match_errors += 1
                    if not match_detail:
                        match_detail = f"koid(s) not recovered: {missing[:3]}"
            gates["compiler"] = _gate(compile_errors == 0,
                                      "pass" if compile_errors == 0 else "fail",
                                      compile_errors)
            gates["execution"] = _gate(exec_errors == 0,
                                       "pass" if exec_errors == 0 else "fail",
                                       exec_errors)
            gates["scenario_match"] = _gate(match_errors == 0,
                                            "pass" if match_errors == 0
                                            else "fail",
                                            match_errors, match_detail)

    # -- determinism -------------------------------------------------------------
    if reference is not None:
        try:
            ref = read_dataset(reference)
        except DatasetError as e:
            gates["determinism"] = _gate(False, "fail", 1,
                                         f"reference unreadable: {e}")
        else:
            same = ref["manifest"] == manifest
            if same:
                from pathlib import Path
                for name in _SPLITS:
                    if (Path(dataset, manifest["splits"][name]["file"])
                            .read_bytes()
                            != Path(reference,
                                    ref["manifest"]["splits"][name]["file"])
                            .read_bytes()):
                        same = False
                        break
            gates["determinism"] = _gate(same, "pass" if same else "fail",
                                         0 if same else 1)
    else:
        gates["determinism"] = _gate(True, "skipped")

    publishable = all(
        g["ok"] for g in gates.values()
        if g["status"] not in ("skipped", "disabled"))
    return {"publishable": publishable, "gates": gates,
            "example_count": manifest["example_count"]}
