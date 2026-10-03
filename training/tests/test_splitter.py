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
