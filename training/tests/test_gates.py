"""T-12 RED: dataset validator + gates (design §26).

validate_dataset enforces every §5 gate fail-closed: a poisoned
dataset passes only when the gate that should catch it is missing —
each test below plants exactly one poison and expects
publishable=False with that gate failing. The static gates run on the
dataset artifact alone; the live gates (compiler, execution,
scenario_match) run only with a db and are reported "skipped"
otherwise. Determinism compares the artifact against a reference
dataset directory. The leakage gate recomputes the split assignment
from the manifest seed — recorded placement must agree AND the
cross-holdout koid pair count must be zero.

Every test below fails against the current tree:
`aikoql_training.dataset.gates` does not exist.
"""

from __future__ import annotations

from aikoql_training.dataset.gates import validate_dataset
from aikoql_training.dataset.splitter import assign_splits
from aikoql_training.dataset.writer import write_dataset
from aikoql_training.models import compute_id
from aikoql_training.validation.grounding import evidence_id
from conftest import make_example

_EV = {"document_id": "d1", "extractor": "e"}
_CREATED = "2026-10-03T00:00:00Z"
_SPLITS = ("train", "val", "test")
_FIELDS = {"dataset_id": "poc-1", "seed": 7, "snapshot_id": "snap-1",
           "configuration_hash": "c" * 64, "created_at": _CREATED}


def _ex(split_key="k", question=None, facts=None, koids=None,
         context=None, **overrides):
    """A grounding-valid grounded_qa example."""
    if question is None:
        question = "What is the owner of the settlement service?"
    if facts is None:
        facts = [{"statement": "The owner of the settlement service "
                              "is Payments Team", "evidence": _EV}]
    ctx = {"entities": [], "relations": [], "evidence": [_EV],
           "facts": facts}
    if context is not None:
        ctx.update(context)
    return make_example(
        input={"question": question},
        context=ctx,
        expected={"answer": "Payments Team", "koids": koids or [],
                  "evidence_ids": [evidence_id(_EV)]},
        split_key=split_key,
        **overrides,
    )


def _dataset(tmp_path, examples, **fields):
    write_dataset({"train": examples, "val": [], "test": []},
                  str(tmp_path), **{**_FIELDS, **fields})
    return str(tmp_path)


def _clean(tmp_path):
    return _dataset(tmp_path, [_ex()])


def test_clean_dataset_passes_all_static_gates(tmp_path):
    out = validate_dataset(_clean(tmp_path))
    assert out["publishable"] is True
    for name in ("schema", "grounding", "authorization", "secret_scan",
                 "leakage", "duplicates"):
        assert out["gates"][name]["ok"], name
    for name in ("compiler", "execution", "scenario_match", "determinism"):
        assert out["gates"][name]["status"] == "skipped", name


# -- schema ------------------------------------------------------------------

def test_invalid_schema_poisons_publishability(tmp_path):
    bad = _ex()
    bad["not_a_schema_field"] = "x"
    out = validate_dataset(_dataset(tmp_path, [bad]))
    assert out["publishable"] is False
    assert out["gates"]["schema"]["ok"] is False


# -- grounding ---------------------------------------------------------------

def test_ungrounded_example_poisons_publishability(tmp_path):
    bad = _ex(facts=[], labels={"grounded": True, "answerable": True,
                                "ambiguous": False, "contradictory": False})
    out = validate_dataset(_dataset(tmp_path, [bad]))
    assert out["publishable"] is False
    assert out["gates"]["grounding"]["ok"] is False


# -- authorization -----------------------------------------------------------

def test_authorization_flag_mismatch_poisons_publishability(tmp_path):
    bad = _ex(
        task={"type": "authorization", "difficulty": "factual", "requires": []},
        policy={"authorization_required": False},
    )
    out = validate_dataset(_dataset(tmp_path, [bad]))
    assert out["publishable"] is False
    assert out["gates"]["authorization"]["ok"] is False


# -- secrets -----------------------------------------------------------------

def test_secret_in_context_poisons_publishability(tmp_path):
    bad = _ex(facts=[{"statement": "The token is sk-live-9f8b7a6c5d4e3f2a1b0c9d8e",
                      "evidence": _EV}])
    out = validate_dataset(_dataset(tmp_path, [bad]))
    assert out["publishable"] is False
    assert out["gates"]["secret_scan"]["ok"] is False


def test_ordinary_text_is_not_a_secret(tmp_path):
    out = validate_dataset(_clean(tmp_path))
    assert out["gates"]["secret_scan"]["ok"] is True


# -- leakage -----------------------------------------------------------------

def _apart_seed(a_key, b_key):
    for seed in range(200):
        splits, _ = assign_splits([_ex(split_key=a_key), _ex(split_key=b_key)],
                                  seed, (8, 1, 1))
        homes = {e["split_key"]: n for n in _SPLITS for e in splits[n]}
        if homes[a_key] != homes[b_key]:
            return seed
    raise AssertionError("no separating seed found")


def test_cross_holdout_koid_sharing_poisons_publishability(tmp_path):
    a = _ex(split_key="k-1", koids=["a" * 32])
    b = _ex(split_key="k-2", koids=["a" * 32],
            question="What is the owner of the checkout service?")
    seed = _apart_seed("k-1", "k-2")
    out = validate_dataset(_dataset(tmp_path, [a, b], seed=seed))
    assert out["publishable"] is False
    assert out["gates"]["leakage"]["ok"] is False


def test_misplaced_split_poisons_the_leakage_gate(tmp_path):
    # the example's recorded split disagrees with the recomputed home
    # (the writer only materializes the three canonical split files)
    a = _ex(split_key="k-1")
    seed = 7
    splits, _ = assign_splits([a], seed, (8, 1, 1))
    home = next(n for n in _SPLITS if splits[n])
    wrong = next(n for n in _SPLITS if n != home)
    write_dataset({n: ([a] if n == wrong else []) for n in _SPLITS},
                  str(tmp_path), **{**_FIELDS, "seed": seed})
    out = validate_dataset(str(tmp_path))
    assert out["publishable"] is False
    assert out["gates"]["leakage"]["ok"] is False


# -- duplicates --------------------------------------------------------------

def test_duplicate_example_ids_poison_publishability(tmp_path):
    a = _ex(split_key="k-1")
    b = dict(a)
    out = validate_dataset(_dataset(tmp_path, [a, b]))
    assert out["publishable"] is False
    assert out["gates"]["duplicates"]["ok"] is False


# -- determinism -------------------------------------------------------------

def test_determinism_reference_mismatch_poisons_publishability(tmp_path):
    a_dir = _dataset(tmp_path / "a", [_ex()])
    b_dir = _dataset(tmp_path / "b", [_ex(), _ex(split_key="k-2")])
    out = validate_dataset(a_dir, reference=b_dir)
    assert out["publishable"] is False
    assert out["gates"]["determinism"]["ok"] is False


def test_determinism_reference_match_passes(tmp_path):
    a_dir = _dataset(tmp_path / "a", [_ex()])
    b_dir = _dataset(tmp_path / "b", [_ex()])
    out = validate_dataset(a_dir, reference=b_dir)
    assert out["gates"]["determinism"]["ok"] is True


# -- live gates --------------------------------------------------------------

def test_live_gates_evaluated_against_a_real_server(mcp_server, tmp_path):
    host, token = mcp_server
    good = _ex()
    bad_query = _ex(question="What is the owner of the gateway service?",
                    query_target={"language": "aikoql", "query": "((("},
                    split_key="k-bad")
    out = validate_dataset(_dataset(tmp_path, [good, bad_query]),
                           db=host, token=token)
    assert out["gates"]["compiler"]["ok"] is False
    assert out["gates"]["execution"]["ok"] is False
    assert out["gates"]["scenario_match"]["ok"] is False
    assert out["publishable"] is False


def test_live_gates_pass_when_queries_recover_their_koids(mcp_server,
                                                          tmp_path):
    host, token = mcp_server
    import aikoql
    with aikoql.Agent.connect(host, token=token) as db:
        koid = db.remember("service", {"name": "settlement",
                                       "owner": "Payments Team"})["koid"]
        ex = _ex(koids=[koid],
                 query_target={"language": "aikoql",
                               "query": 'MATCH service WHERE name == '
                                        '"settlement" RETURN owner'})
        out = validate_dataset(_dataset(tmp_path, [ex]), db=host, token=token)
    assert out["gates"]["compiler"]["ok"] is True
    assert out["gates"]["execution"]["ok"] is True
    assert out["gates"]["scenario_match"]["ok"] is True
    assert out["publishable"] is True


def test_scenario_match_fails_on_a_missing_koid(mcp_server, tmp_path):
    host, token = mcp_server
    ex = _ex(koids=["f" * 32])
    out = validate_dataset(_dataset(tmp_path, [ex]), db=host, token=token)
    assert out["gates"]["scenario_match"]["ok"] is False
    assert out["publishable"] is False


# -- leakage dimensions (T-19: PR9 Finding #4 / TDD-10 / FZ-08) ---------------

def _placed(tmp_path, examples, seed):
    """Write each example into its RECOMPUTED home so the leakage gate
    has no misplaced noise — the planted dimension is the only poison."""
    splits, _ = assign_splits(examples, seed, (8, 1, 1))
    homes = {e["example_id"]: n for n in _SPLITS for e in splits[n]}
    write_dataset({n: [e for e in examples if homes[e["example_id"]] == n]
                   for n in _SPLITS},
                  str(tmp_path), **{**_FIELDS, "seed": seed})
    return str(tmp_path)


def test_canonical_question_crossing_the_holdout_poisons_publishability(
        tmp_path):
    """TDD-10: the SAME question text in two splits is the review's
    canonical leakage — the model saw the answer in train, so the test
    split can never measure it. The canonical tooth is HARD."""
    # same question text from TWO scenarios (distinct entities):
    # example_id keys on the question AND the scenario_id, so this is
    # not a duplicate — but the model has seen the exact question in
    # one split and the other can never measure it again.
    a = _ex(split_key="k-1", koids=["a" * 32])
    b = _ex(split_key="k-2", koids=["b" * 32])
    b["source"]["scenario_id"] = "factual:policy:p-02"
    b["example_id"] = compute_id(b)
    seed = _apart_seed("k-1", "k-2")
    out = validate_dataset(_placed(tmp_path, [a, b], seed))
    assert out["publishable"] is False
    assert out["gates"]["leakage"]["ok"] is False
    assert out["gates"]["leakage"]["dimensions"]["canonical"] >= 1


def test_diagnostic_dimensions_report_but_never_veto(tmp_path):
    """Answer + entity-name overlap across splits is REPORTED, never
    vetoed: template corpora share answers and names structurally
    ("Payments Team" owns many services) — the review's own caveat
    keeps these diagnostic."""
    a = _ex(split_key="k-1",
            context={"entities": [{"koid": "a" * 32, "name":
                                   "settlement"}]})
    b = _ex(split_key="k-2",
            question="What is the owner of the checkout service?",
            context={"entities": [{"koid": "b" * 32, "name":
                                   "settlement"}]})
    seed = _apart_seed("k-1", "k-2")
    out = validate_dataset(_placed(tmp_path, [a, b], seed))
    assert out["publishable"] is True
    assert out["gates"]["leakage"]["ok"] is True
    assert out["gates"]["leakage"]["dimensions"]["identifier"] >= 1
    assert out["gates"]["leakage"]["dimensions"]["answer"] >= 1
    assert out["gates"]["leakage"]["dimensions"]["canonical"] == 0


def test_clean_dataset_reports_zero_leakage_dimensions(tmp_path):
    out = validate_dataset(_clean(tmp_path))
    assert out["gates"]["leakage"]["dimensions"] == {
        "canonical": 0, "identifier": 0, "normalized": 0,
        "answer": 0, "relation_pattern": 0}


def test_leakage_dimensions_are_shuffle_invariant():
    """FZ-08: the dimensions report is a pure function of the example
    set — input order can never change the counts."""
    from aikoql_training.dataset.gates import _leakage_dimensions
    a = _ex(split_key="k-1")
    b = _ex(split_key="k-2",
            question="What is the owner of the checkout service?")
    c = _ex(split_key="k-3",
            question="Which service does settlement depend on?")
    exs = [a, b, c]
    splits, _ = assign_splits(exs, 7, (8, 1, 1))
    baseline = _leakage_dimensions(splits, exs)
    for perm in ([c, a, b], [b, c, a]):
        splits2, _ = assign_splits(perm, 7, (8, 1, 1))
        assert _leakage_dimensions(splits2, perm) == baseline
