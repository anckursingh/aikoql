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
from aikoql_training.validation.grounding import evidence_id
from conftest import make_example

_EV = {"document_id": "d1", "extractor": "e"}
_CREATED = "2026-10-03T00:00:00Z"
_SPLITS = ("train", "val", "test")
_FIELDS = {"dataset_id": "poc-1", "seed": 7, "snapshot_id": "snap-1",
           "configuration_hash": "c" * 64, "created_at": _CREATED}


def _ex(split_key="k", question=None, facts=None, koids=None, **overrides):
    """A grounding-valid grounded_qa example."""
    if question is None:
        question = "What is the owner of the settlement service?"
    if facts is None:
        facts = [{"statement": "The owner of the settlement service "
                              "is Payments Team", "evidence": _EV}]
    return make_example(
        input={"question": question},
        context={"entities": [], "relations": [], "evidence": [_EV],
                 "facts": facts},
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
    for name in ("schema", "grounding", "authorization", "secrets",
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
    assert out["gates"]["secrets"]["ok"] is False


def test_ordinary_text_is_not_a_secret(tmp_path):
    out = validate_dataset(_clean(tmp_path))
    assert out["gates"]["secrets"]["ok"] is True


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
