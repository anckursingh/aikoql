"""T-11 RED: knowledge-level splitter (design Phase 13).

The splitter assigns every example to train/val/test by its
`split_key` — the holdout dimension the example belongs to (schema
field since T-01). Assignment is a seeded stable hash of the key
alone, so near-duplicate questions (template variants of the same
fact) sharing a key can never straddle a holdout, and no input
shuffle can move an example. The splitter also reports cross-holdout
violations: two examples in different splits that share any
expected.koid leak knowledge across the holdout boundary (the §26
leakage gate counts them).

Every test below fails against the current tree:
`aikoql_training.dataset.splitter` does not exist.
"""

from __future__ import annotations

from hypothesis import given, settings
from hypothesis import strategies as st

from aikoql_training.dataset.splitter import assign_splits
from conftest import make_example

_RATIOS = (8, 1, 1)
_SEEDS = (0, 7, 12345)


def _ex(split_key="k", koids=None, question=None):
    return make_example(
        input={"question": question or "What is the owner of the "
                 "settlement service?"},
        expected={"answer": "Payments Team", "koids": koids or [],
                  "evidence_ids": []},
        split_key=split_key,
    )


# -- assignment -------------------------------------------------------------

def test_near_duplicate_questions_never_straddle_a_holdout():
    a = _ex(split_key="settlement-owner")
    b = _ex(split_key="settlement-owner",
            question="Who owns the settlement service?")
    for seed in _SEEDS:
        splits, _ = assign_splits([a, b], seed, _RATIOS)
        homes = [name for name in ("train", "val", "test")
                 if any(e["example_id"] == a["example_id"] for e in splits[name])]
        assert homes == [name for name in ("train", "val", "test")
                         if any(e["example_id"] == b["example_id"] for e in splits[name])]


def test_assignment_is_seed_stable_and_input_order_invariant():
    examples = [_ex(split_key=f"k{i:03d}") for i in range(40)]
    baseline = assign_splits(examples, 7, _RATIOS)[0]
    shuffled = assign_splits(list(reversed(examples)), 7, _RATIOS)[0]
    for name in ("train", "val", "test"):
        assert {e["example_id"] for e in baseline[name]} == {
            e["example_id"] for e in shuffled[name]}


def test_ratios_are_respected_in_expectation():
    examples = [_ex(split_key=f"k{i:03d}") for i in range(40)]
    splits, _ = assign_splits(examples, 7, _RATIOS)
    counts = {n: len(splits[n]) for n in ("train", "val", "test")}
    assert sum(counts.values()) == 40
    assert all(c > 0 for c in counts.values())
    assert counts["train"] == max(counts.values())


def test_empty_input_gives_three_empty_splits():
    splits, violations = assign_splits([], 7, _RATIOS)
    assert all(len(splits[n]) == 0 for n in ("train", "val", "test"))
    assert violations == []


def test_malformed_ratios_are_rejected():
    from aikoql_training.errors import DatasetError
    ex = _ex()
    for bad in ((0, 0, 0), (-1, 1, 1), (8, 1)):
        try:
            assign_splits([ex], 7, bad)
        except DatasetError:
            continue
        raise AssertionError(f"ratios {bad!r} accepted")


# -- cross-holdout leakage --------------------------------------------------

def _home(splits, example):
    return next(name for name in ("train", "val", "test")
                if any(e["example_id"] == example["example_id"]
                       for e in splits[name]))


def test_shared_koid_across_splits_is_reported():
    a = _ex(split_key="k-1", koids=["a" * 32])
    b = _ex(split_key="k-2", koids=["a" * 32],
            question="What is the owner of the checkout service?")
    for seed in _SEEDS:
        splits, violations = assign_splits([a, b], seed, _RATIOS)
        if _home(splits, a) == _home(splits, b):
            continue  # same split — no violation this seed
        assert violations, f"seed {seed}: shared koid across splits unreported"
        _, _, shared = violations[0]
        assert shared == "a" * 32


def test_same_split_koid_sharing_is_not_a_violation():
    a = _ex(split_key="k-1", koids=["a" * 32])
    b = _ex(split_key="k-1", koids=["a" * 32],
            question="Who owns the settlement service?")
    for seed in _SEEDS:
        _, violations = assign_splits([a, b], seed, _RATIOS)
        assert violations == []


# -- component keys (the T-13 fix: mixed-cardinality koid sets) -------------

def test_component_keys_make_mixed_koid_sets_violation_free():
    """Factual examples key on one koid, relation examples on both —
    the koid-set join makes different KEYS for the same knowledge
    component, and the bucket lottery straddles them. Component ids
    (union-find over the edges, root = min koid) give every example
    touching a component the SAME key, so cross-holdout koid pairs
    are impossible under EVERY seed."""
    from aikoql_training.dataset.splitter import component_ids
    s, c = "b" * 32, "a" * 32
    ids = component_ids([{"from": s, "rel": "DEPENDS_ON", "to": c}])
    assert ids[s] == ids[c] == "a" * 32  # root = min koid
    key = ids[s]
    for seed in range(50):
        examples = [
            _ex(split_key=key, koids=[s]),
            _ex(split_key=key, koids=[c],
                question="What is the owner of the checkout service?"),
            _ex(split_key=key, koids=[s, c],
                question="Which service does settlement depend on?"),
        ]
        _, violations = assign_splits(examples, seed, _RATIOS)
        assert violations == [], f"seed {seed}"


def test_koid_set_join_keys_do_straddle_some_seed():
    """The trap the component fix closes: with per-example koid-set
    join keys, the mixed sets {s} and {s,c} hash to different buckets
    and some seed produces cross-holdout pairs (the T-12 hole, caught
    by the leakage gate on a live generate run)."""
    s, c = "b" * 32, "a" * 32
    straddles = 0
    for seed in range(50):
        examples = [
            _ex(split_key=s, koids=[s]),
            _ex(split_key=c, koids=[c],
                question="What is the owner of the checkout service?"),
            _ex(split_key=":".join(sorted((s, c))), koids=[s, c],
                question="Which service does settlement depend on?"),
        ]
        _, violations = assign_splits(examples, seed, _RATIOS)
        straddles += bool(violations)
    assert straddles > 0


@settings(max_examples=25)
@given(
    keys=st.lists(st.text(min_size=1, max_size=8),
                  min_size=1, max_size=10, unique=True),
    seed=st.integers(min_value=0, max_value=2**31 - 1),
)
def test_assignment_depends_only_on_split_key_and_seed(keys, seed):
    examples = [_ex(split_key=k) for k in keys]
    baseline = assign_splits(examples, seed, _RATIOS)
    for perm in (list(reversed(examples)), examples[1:] + examples[:1]):
        splits, _ = assign_splits(perm, seed, _RATIOS)
        for name in ("train", "val", "test"):
            assert {e["example_id"] for e in baseline[0][name]} == {
                e["example_id"] for e in splits[name]}

# -- group union (T-19: ambiguity groups are one knowledge component) --------

def test_component_ids_unions_a_group_across_components():
    """PR9 Finding #4: two KOs sharing an anchor value (an ambiguity
    group) are the same knowledge even with NO edge between them. The
    group must land in ONE component or ambiguity + factual examples
    about its members can straddle the holdout under some seed."""
    from aikoql_training.dataset.splitter import component_ids
    a, b, c, d = ("k0" + "0" * 30, "k1" + "0" * 30,
                  "k2" + "0" * 30, "k3" + "0" * 30)
    ids = component_ids(
        [{"from": a, "rel": "DEPENDS_ON", "to": b},
         {"from": c, "rel": "DEPENDS_ON", "to": d}],
        groups=[(b, c)],
    )
    assert {ids[a], ids[b], ids[c], ids[d]} == {a}  # one root: min koid


def test_component_ids_unions_edgeless_group_members():
    """Group members with no edges at all still form a component: the
    ambiguity group is knowledge even before any relation exists."""
    from aikoql_training.dataset.splitter import component_ids
    a, b = "a" + "0" * 31, "b" + "0" * 31
    ids = component_ids([], groups=[(a, b)])
    assert ids[a] == ids[b] == a
