"""Agent-embedded parity pins.

The Agent wrapper is the MRFC-0040 unified interface; embedded mode must
behave like the native surface. Authored RED (2026-09-16): the wrapper
passed (type_name, properties, koid, subject, note) into the native
(subject, type_name, properties, semantic, roles) signature — every
embedded call through Agent failed — and the native remember had no koid
update path (the MCP mode's documented update surface).

Embedded identity: the process owner (subject "owner"), matching
session_init's contract.
"""

import pytest

from aikoql import Agent


@pytest.fixture
def db(tmp_path):
    agent = Agent.connect(str(tmp_path / "kb"))
    yield agent
    agent.close()


def test_embedded_remember_get_roundtrip(db):
    r = db.remember("note", {"topic": "pet", "body": "cats"})
    ko = db.get(r["koid"])
    assert ko["type_name"] == "note"
    assert ko["properties"]["topic"] == "pet"
    assert ko["properties"]["body"] == "cats"


def test_embedded_remember_with_koid_updates(db):
    r = db.remember("note", {"topic": "pet", "body": "cats", "seq": 1})
    upd = db.remember(
        "note", {"topic": "pet", "body": "cats.v2", "seq": 1}, koid=r["koid"]
    )
    assert upd["koid"] == r["koid"]
    ko = db.get(r["koid"])
    assert ko["properties"]["body"] == "cats.v2"
    # Kernel update REPLACES the property map — the restated fields survive.
    assert ko["properties"]["topic"] == "pet"
    assert ko["version"] == 2


def test_embedded_relate_and_traverse(db):
    a = db.remember("note", {"body": "A"})
    b = db.remember("event", {"label": "e0"})
    rel = db.relate(a["koid"], b["koid"], "mentions")
    assert rel["koid"] == a["koid"]
    hits = db.traverse(a["koid"], "mentions", 1)
    assert [h["koid"] for h in hits] == [b["koid"]]
    assert hits[0]["depth"] == 1


def test_embedded_vector_roundtrip(db):
    cats = db.remember(
        "note", {"topic": "pet", "body": "cats"},
        semantic={"embedding": [1.0, 0.0]},
    )
    db.remember(
        "note", {"topic": "pet", "body": "dogs"},
        semantic={"embedding": [0.0, 1.0]},
    )
    hits = db.find_similar(vector=[1.0, 0.0], k=2, fusion="vector_only")
    assert len(hits) == 2
    assert hits[0]["koid"] == cats["koid"]


def test_embedded_aikoql_sees_own_rows(db):
    db.remember("note", {"topic": "pet", "body": "cats"})
    out = db.aikoql('MATCH note WHERE topic == "pet" RETURN *')
    assert len(out) == 1
    assert out[0]["properties"]["body"] == "cats"
