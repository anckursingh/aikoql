"""T-02 RED — the snapshot adapter (design Phase 2 + §10).

Dataset generation must be snapshot-based: every example's source block
traces to a captured state (database identity + knowledge revision +
configuration + seed). The adapter talks to the real public surface —
the MCP health tool — because embedded Agent.health() is a stub
(recon §4).

RED (design's Phase 2 list, this file fails against the current tree):
  1. database identity captured
  2. knowledge revision captured (journal_seq + audit_hash)
  3. configuration captured (canonical hash)
  4. seed captured
  5. source manifest captured
  6. manifest reproducibility — same state => same snapshot identity
     (the design's acceptance), canonical serialization, created_at
     excluded from identity
  7. a live state change moves the identity
  8. the snapshot feeds the canonical example schema (T-01 cross-check)
"""

import json

import pytest
from aikoql import Agent

from aikoql_training.client import capture_from_agent
from aikoql_training.models import GENERATOR_VERSION, SCHEMA_VERSION, validate
from aikoql_training.snapshot import Snapshot, capture_snapshot, snapshot_to_json
from conftest import make_example

_HEALTH = {"journal_seq": 42, "audit_hash": "ab" * 32}


def test_captures_database_identity():
    snap = capture_snapshot("acmepay", _HEALTH)
    assert snap.database_id == "acmepay"
    assert snap.snapshot_id.startswith("sha256:")
    assert len(snap.snapshot_id) == len("sha256:") + 64


def test_empty_database_id_refused():
    with pytest.raises(ValueError, match="database_id"):
        capture_snapshot("", _HEALTH)


def test_captures_knowledge_revision():
    snap = capture_snapshot("acmepay", _HEALTH)
    assert snap.knowledge_revision == "42:" + "ab" * 32


def test_health_missing_keys_refused():
    with pytest.raises(ValueError, match="journal_seq"):
        capture_snapshot("acmepay", {"audit_hash": "ab" * 32})
    with pytest.raises(ValueError, match="audit_hash"):
        capture_snapshot("acmepay", {"journal_seq": 1})


def test_captures_configuration_hash():
    config = {"count": 100, "mix": {"factual": 5, "relation": 3}}
    snap = capture_snapshot("acmepay", _HEALTH, config=config)
    # Canonical: key order must not matter.
    reordered = {"mix": {"relation": 3, "factual": 5}, "count": 100}
    other = capture_snapshot("acmepay", _HEALTH, config=reordered)
    assert snap.configuration_hash == other.configuration_hash
    assert snap.configuration_hash.startswith("sha256:")
    changed = capture_snapshot("acmepay", _HEALTH, config={"count": 101})
    assert changed.configuration_hash != snap.configuration_hash


def test_captures_seed():
    snap = capture_snapshot("acmepay", _HEALTH, seed=7)
    assert snap.seed == 7
    assert capture_snapshot("acmepay", _HEALTH).seed == 0


def test_captures_source_manifest_hash():
    manifest = ["factual:policy:p-01", "relation:ownership:o-02"]
    snap = capture_snapshot("acmepay", _HEALTH, source_manifest=manifest)
    assert snap.source_manifest_hash.startswith("sha256:")
    reordered = list(reversed(manifest))
    assert (
        capture_snapshot("acmepay", _HEALTH, source_manifest=reordered).source_manifest_hash
        == snap.source_manifest_hash
    )


def test_same_state_same_identity():
    # The design's acceptance: two snapshots of the same immutable state
    # produce the same identity. created_at is metadata, not identity.
    a = capture_snapshot("acmepay", _HEALTH, now="2026-10-03T00:00:00Z")
    b = capture_snapshot("acmepay", _HEALTH, now="2026-10-04T09:30:00Z")
    assert a.snapshot_id == b.snapshot_id
    assert a.created_at != b.created_at
    assert a.schema_version == SCHEMA_VERSION
    assert a.generator_version == GENERATOR_VERSION


def test_state_change_moves_identity():
    base = capture_snapshot("acmepay", _HEALTH)
    moved = capture_snapshot("acmepay", {"journal_seq": 43, "audit_hash": "ab" * 32})
    assert moved.snapshot_id != base.snapshot_id
    assert moved.knowledge_revision != base.knowledge_revision


def test_manifest_reproducibility():
    snap = capture_snapshot("acmepay", _HEALTH, config={"count": 1}, seed=3)
    blob = snapshot_to_json(snap)
    assert "\n" not in blob
    assert json.loads(blob) == snap.to_dict()


def test_snapshot_feeds_example_source():
    snap = capture_snapshot("acmepay", _HEALTH, seed=3)
    example = make_example(
        source={
            "database_id": snap.database_id,
            "snapshot_id": snap.snapshot_id,
            "knowledge_revision": snap.knowledge_revision,
            "scenario_id": "factual:policy:p-01",
            "created_at": snap.created_at,
        }
    )
    validate(example)  # the T-01 schema accepts the real snapshot block


def test_live_snapshot_over_real_server(mcp_server):
    """The adapter over the real public surface: MCP health tool."""
    host, token = mcp_server
    with Agent.connect(host, token=token) as db:
        db.remember("note", {"topic": "snap", "body": "v1"})
        health = db.health()
        snap = capture_from_agent(db, "acmepay", seed=1)
    assert snap.knowledge_revision == (
        f"{health['journal_seq']}:{health['audit_hash']}"
    )
    assert len(health["audit_hash"]) == 64
    assert health["journal_seq"] > 0
    # Same state captured twice over the live server: same identity.
    with Agent.connect(host, token=token) as db:
        again = capture_from_agent(db, "acmepay", seed=1)
    assert again.snapshot_id == snap.snapshot_id
