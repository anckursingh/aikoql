"""T-11 RED: dataset writer + reader (design Phase 14).

The writer is the publication boundary. Every example line is the
canonical single-line `to_json` (sort_keys, tight separators,
ensure_ascii=False — models.py), sorted by example_id within each
split; split files are written to temp names and atomically renamed
(os.replace), stale temp files from an interrupted run are swept at
start, and manifest.json is written LAST — its presence is dataset
visibility. The manifest carries per-split count/file/sha256 plus
example_count and the identity fields (dataset_id, schema_version,
generator_version, seed, snapshot_id, configuration_hash,
created_at) — `created_at` is an EXPLICIT operator parameter so the
same inputs regenerate byte-identical output (determinism law).

The reader verifies the manifest sha256 + counts and refuses any
tampered/truncated dataset (fail-closed, DatasetError) — FZ-T2.

Every test below fails against the current tree:
`aikoql_training.dataset.writer` does not exist.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from hypothesis import given, settings
from hypothesis import strategies as st

from aikoql_training.dataset.writer import read_dataset, write_dataset
from aikoql_training.errors import DatasetError
from conftest import make_example

_CREATED = "2026-10-03T00:00:00Z"
_SPLITS = ("train", "val", "test")
_TEXT = st.text(alphabet=st.characters(
    min_codepoint=1, max_codepoint=0x10FFFF), max_size=24)


def _ex(split_key="k", question=None, answer=None):
    return make_example(
        input={"question": question or "What is the owner of the "
                 "settlement service?"},
        expected={"answer": answer or "Payments Team", "koids": [],
                  "evidence_ids": []},
        split_key=split_key,
    )


def _write(tmp_path, examples, **fields):
    fields = {"dataset_id": "poc-1", "seed": 7,
              "snapshot_id": "snap-1", "configuration_hash": "c" * 64,
              "created_at": _CREATED, **fields}
    write_dataset(
        {"train": examples, "val": [], "test": []},
        str(tmp_path), **fields)
    return fields


# -- round trip -------------------------------------------------------------

def test_write_then_read_roundtrips_exactly():
    import aikoql_training.models as m
    examples = [_ex(split_key=f"k{i}") for i in range(5)]
    fields = _write(tmp_path, examples)
    got = read_dataset(str(tmp_path))
    assert got["manifest"]["example_count"] == 5
    assert got["manifest"]["splits"]["train"]["count"] == 5
    assert got["train"] == sorted(examples, key=lambda e: e["example_id"])
    assert got["val"] == [] and got["test"] == []
    for name in _SPLITS:
        cell = got["manifest"]["splits"][name]
        raw = Path(tmp_path, cell["file"]).read_bytes()
        assert hashlib.sha256(raw).hexdigest() == cell["sha256"]
    for k in ("dataset_id", "seed", "snapshot_id", "configuration_hash",
              "created_at"):
        assert got["manifest"][k] == fields[k]
    assert got["manifest"]["schema_version"] == m.SCHEMA_VERSION


def test_examples_are_sorted_by_id_regardless_of_input_order():
    examples = [_ex(split_key=f"k{i}") for i in range(3)]
    d1, d2 = tmp_path / "a", tmp_path / "b"
    _write(d1, examples)
    _write(d2, list(reversed(examples)))
    assert (d1 / "train.jsonl").read_bytes() == \
        (d2 / "train.jsonl").read_bytes()


# -- determinism law --------------------------------------------------------

def test_regeneration_is_byte_identical():
    examples = [_ex(split_key=f"k{i}") for i in range(6)]
    _write(tmp_path / "a", examples)
    _write(tmp_path / "b", examples)
    for p in (tmp_path / "a").iterdir():
        assert p.read_bytes() == (tmp_path / "b" / p.name).read_bytes()


# -- refusal (FZ-T2) --------------------------------------------------------

def test_missing_dataset_dir_refused():
    try:
        read_dataset(str(tmp_path / "nope"))
    except DatasetError:
        return
    raise AssertionError("missing dataset dir accepted")


def test_missing_manifest_refused():
    (tmp_path / "train.jsonl").write_text("", encoding="utf-8")
    try:
        read_dataset(str(tmp_path))
    except DatasetError:
        return
    raise AssertionError("dataset without manifest accepted")


def test_tampered_split_file_refused():
    _write(tmp_path, [_ex()])
    p = tmp_path / "train.jsonl"
    p.write_bytes(p.read_bytes()[:-1] + b"x")
    try:
        read_dataset(str(tmp_path))
    except DatasetError as e:
        assert "sha256" in str(e)
        return
    raise AssertionError("tampered split file accepted")


def test_truncated_split_file_refused():
    _write(tmp_path, [_ex()])
    p = tmp_path / "train.jsonl"
    p.write_bytes(p.read_bytes()[:5])
    try:
        read_dataset(str(tmp_path))
    except DatasetError:
        return
    raise AssertionError("truncated split file accepted")


def test_tampered_manifest_refused():
    _write(tmp_path, [_ex()])
    p = tmp_path / "manifest.json"
    m = json.loads(p.read_text(encoding="utf-8"))
    m["splits"]["train"]["sha256"] = "0" * 64
    p.write_text(json.dumps(m), encoding="utf-8")
    try:
        read_dataset(str(tmp_path))
    except DatasetError:
        return
    raise AssertionError("tampered manifest accepted")


def test_manifest_count_mismatch_refused():
    _write(tmp_path, [_ex()])
    p = tmp_path / "manifest.json"
    m = json.loads(p.read_text(encoding="utf-8"))
    m["splits"]["train"]["count"] = 99
    p.write_text(json.dumps(m), encoding="utf-8")
    try:
        read_dataset(str(tmp_path))
    except DatasetError:
        return
    raise AssertionError("count mismatch accepted")


@settings(max_examples=25)
@given(
    data=st.binary(min_size=1, max_size=200),
)
def test_any_arbitrary_split_content_is_refused(data):
    """FZ-T2 law: a mutated split file either refuses or round-trips —
    never a silent wrong read."""
    _write(tmp_path, [_ex()])
    (tmp_path / "train.jsonl").write_bytes(data)
    try:
        got = read_dataset(str(tmp_path))
    except DatasetError:
        return
    assert got["manifest"]["splits"]["train"]["count"] == len(got["train"])
    for e in got["train"]:
        # whatever parses must be real example lines (no fields invented)
        assert "example_id" in e and e["example_id"]


# -- atomicity / interrupted-run cleanup ------------------------------------

def test_stale_temp_files_swept_and_no_tmp_left():
    (tmp_path / "train.jsonl.tmp123").write_text("stale", encoding="utf-8")
    (tmp_path / "manifest.json.tmp9").write_text("stale", encoding="utf-8")
    _write(tmp_path, [_ex()])
    leftovers = [p.name for p in tmp_path.iterdir() if ".tmp" in p.name]
    assert leftovers == []
    assert {p.name for p in tmp_path.iterdir()} == {
        "train.jsonl", "val.jsonl", "test.jsonl", "manifest.json"}


def test_empty_split_still_gets_a_file():
    _write(tmp_path, [])
    got = read_dataset(str(tmp_path))
    assert got["manifest"]["example_count"] == 0
    for name in _SPLITS:
        assert Path(tmp_path, got["manifest"]["splits"][name]["file"]).exists()


# -- adversarial round-trip (FZ-T6) -----------------------------------------

@settings(max_examples=30)
@given(q=_TEXT, a=_TEXT)
def test_adversarial_content_roundtrips_byte_identical(q, a):
    _write(tmp_path, [_ex(question=q, answer=a)])
    got = read_dataset(str(tmp_path))
    [e] = got["train"]
    assert e["input"]["question"] == q
    assert e["expected"]["answer"] == a
