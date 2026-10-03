"""T-12 RED: the aikoql-training CLI.

Five subcommands over the T-01..T-11 engine: snapshot (capture a
dataset snapshot), generate (the end-to-end pipeline on a fixture DB:
seed → snapshot → scenarios → oracle → context → answers → split →
write → validate → determinism re-run), validate (every §5 gate,
exit 0 = publishable, exit 1 = not), stats (counts and rates),
export (copy a canonical dataset).

Every test below fails against the current tree:
`aikoql_training.cli` does not exist.
"""

from __future__ import annotations

import json

import pytest

from aikoql_training.cli import main
from aikoql_training.dataset.writer import read_dataset, write_dataset
from conftest import make_example

_CREATED = "2026-10-03T00:00:00Z"
_FIELDS = {"dataset_id": "poc-1", "seed": 7, "snapshot_id": "snap-1",
           "configuration_hash": "c" * 64, "created_at": _CREATED}


def _dataset(tmp_path, n=1):
    examples = [make_example(split_key=f"k{i}",
                             input={"question": f"What is the owner of "
                                                f"service {i}?"})
                for i in range(n)]
    write_dataset({"train": examples, "val": [], "test": []},
                  str(tmp_path), **_FIELDS)
    return str(tmp_path)


# -- usage -------------------------------------------------------------------

def test_no_arguments_is_a_usage_error(capsys):
    with pytest.raises(SystemExit) as e:
        main([])
    assert e.value.code == 2


def test_unknown_subcommand_is_a_usage_error(capsys):
    with pytest.raises(SystemExit) as e:
        main(["frobnicate"])
    assert e.value.code == 2


# -- validate ----------------------------------------------------------------

def test_validate_clean_dataset_exits_zero(capsys, tmp_path):
    assert main(["validate", _dataset(tmp_path)]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["publishable"] is True


def test_validate_poisoned_dataset_exits_one(capsys, tmp_path):
    bad = make_example()
    bad["not_a_schema_field"] = "x"
    write_dataset({"train": [bad], "val": [], "test": []},
                  str(tmp_path), **_FIELDS)
    assert main(["validate", str(tmp_path)]) == 1


# -- stats -------------------------------------------------------------------

def test_stats_counts_splits_and_exits_zero(capsys, tmp_path):
    assert main(["stats", _dataset(tmp_path, n=3)]) == 0
    stats = json.loads(capsys.readouterr().out)
    assert stats["example_count"] == 3
    assert stats["splits"]["train"] == 3


# -- export ------------------------------------------------------------------

def test_export_copies_the_canonical_dataset(capsys, tmp_path):
    src = _dataset(tmp_path / "src")
    dst = tmp_path / "dst"
    assert main(["export", src, "--out", str(dst)]) == 0
    got = read_dataset(str(dst))
    assert got["manifest"]["example_count"] == 1
    assert set(p.name for p in dst.iterdir()) == {
        "train.jsonl", "val.jsonl", "test.jsonl", "manifest.json"}


# -- snapshot / generate (live) ----------------------------------------------

def test_snapshot_captures_against_a_real_server(capsys, mcp_server):
    host, token = mcp_server
    assert main(["snapshot", "--db", host, "--token", token,
                 "--database-id", "acmepay"]) == 0
    snap = json.loads(capsys.readouterr().out)
    assert snap["database_id"] == "acmepay"
    assert len(snap["snapshot_id"]) == 64


def test_generate_runs_end_to_end_on_a_fixture_db(capsys, mcp_server,
                                                  tmp_path):
    """The full pipeline over a live fixture DB: examples generated,
    oracle-verified, grounded, split, written, validated — and the
    determinism re-run proves byte-identical regeneration."""
    host, token = mcp_server
    out = tmp_path / "dataset"
    assert main(["generate", "--db", host, "--token", token,
                 "--out", str(out)]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["publishable"] is True
    assert report["gates"]["determinism"]["ok"] is True
    dataset = read_dataset(str(out))
    assert dataset["manifest"]["example_count"] > 0
    assert {p.name for p in out.iterdir()} == {
        "train.jsonl", "val.jsonl", "test.jsonl", "manifest.json"}
