import os
import shutil
import tempfile

import pytest

from aikoql import aikoql
from aikoql.checkpointer import AikoqlCheckpointer


@pytest.fixture
def tmp_aikoql():
    d = tempfile.mkdtemp(prefix="aikoql-py-test-")
    path = os.path.join(d, "test.redb")
    client = aikoql(path, salt=42)
    try:
        yield client
    finally:
        client.close()
        shutil.rmtree(d, ignore_errors=True)


def test_remember_and_get(tmp_aikoql):
    result = tmp_aikoql.remember(
        "alice",
        "fact",
        {"body": "cats are great", "score": 42, "ok": True},
    )
    assert "koid" in result
    assert result["version"] == 1
    assert result["commit_ts"] > 0

    ko = tmp_aikoql.get("alice", result["koid"])
    assert ko["type_name"] == "fact"
    assert ko["properties"]["body"] == "cats are great"
    assert ko["properties"]["score"] == 42
    assert ko["properties"]["ok"] is True


def test_find_similar_text(tmp_aikoql):
    tmp_aikoql.remember(
        "alice", "fact", {"body": "cats and dogs"}
    )
    tmp_aikoql.remember(
        "alice", "fact", {"body": "unrelated fish"}
    )
    hits = tmp_aikoql.find_similar("alice", text="cats", k=5)
    assert len(hits) >= 1
    assert hits[0]["ko"]["properties"]["body"] == "cats and dogs"


def test_checkpointer_roundtrip(tmp_aikoql):
    cp = AikoqlCheckpointer.from_client(tmp_aikoql)

    config = {"configurable": {"thread_id": "thread-1"}}
    checkpoint = {"id": "chk-1", "ts": "123", "channel_values": {"x": 1}}
    new_config = cp.put(config, checkpoint)
    assert new_config["configurable"]["checkpoint_id"] == "chk-1"

    loaded = cp.get(config)
    assert loaded == checkpoint

    cp.put(config, {"id": "chk-2", "ts": "124", "channel_values": {"x": 2}})
    latest = cp.get(config)
    assert latest["id"] == "chk-2"

    all_checks = list(cp.list(config))
    assert len(all_checks) == 2

    cp.close()


def test_relate_and_traverse(tmp_aikoql):
    a = tmp_aikoql.remember("alice", "note", {"body": "A"})
    b = tmp_aikoql.remember("alice", "note", {"body": "B"})
    c = tmp_aikoql.remember("alice", "note", {"body": "C"})

    rel = tmp_aikoql.relate("alice", a["koid"], b["koid"], "references")
    assert rel["koid"] == a["koid"]
    assert rel["version"] == 2

    rel_c = tmp_aikoql.relate("alice", a["koid"], c["koid"], "cites")

    # idempotent: duplicate edge returns current source version, not a new version
    rel2 = tmp_aikoql.relate("alice", a["koid"], b["koid"], "references")
    assert rel2["version"] == rel_c["version"]

    hits = tmp_aikoql.traverse("alice", a["koid"], depth=1)
    assert len(hits) == 2

    filtered = tmp_aikoql.traverse(
        "alice", a["koid"], rel_type="references", depth=1
    )
    assert len(filtered) == 1
    assert filtered[0]["koid"] == b["koid"]
    assert filtered[0]["direction"] == "outbound"


def test_aikoql_match(tmp_aikoql):
    tmp_aikoql.remember("alice", "Person", {"name": "Alice", "city": "Amsterdam"})
    tmp_aikoql.remember("alice", "Person", {"name": "Bob", "city": "London"})

    results = tmp_aikoql.aikoql("MATCH Person RETURN *", subject="alice")
    assert len(results) == 2
    assert results[0]["type_name"] == "Person"
