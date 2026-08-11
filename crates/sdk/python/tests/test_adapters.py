import asyncio
import os
import shutil
import tempfile

import pytest

from aikoql import aikoql, AikoqlCrewAIMemory, AikoqlLangGraphSaver


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


def test_langgraph_saver_roundtrip(tmp_aikoql):
    saver = AikoqlLangGraphSaver.from_client(tmp_aikoql)

    config = {"configurable": {"thread_id": "thread-a"}}
    cp = {"id": "chk-1", "ts": "1", "channel_values": {"x": 1}}
    new_config = saver.put(config, cp)
    assert new_config["configurable"]["checkpoint_id"] == "chk-1"

    assert saver.get(config) == cp

    saver.put(config, {"id": "chk-2", "ts": "2", "channel_values": {"x": 2}})
    latest = saver.get(config)
    assert latest["id"] == "chk-2"

    assert len(list(saver.list(config))) == 2

    by_id = saver.get(new_config)
    assert by_id == cp

    saver.close()


def test_langgraph_saver_async_roundtrip(tmp_aikoql):
    saver = AikoqlLangGraphSaver.from_client(tmp_aikoql)

    async def _run():
        config = {"configurable": {"thread_id": "thread-b"}}
        cp = {"id": "chk-3", "ts": "3", "channel_values": {"y": 9}}
        new_config = await saver.aput(config, cp)
        assert new_config["configurable"]["checkpoint_id"] == "chk-3"
        assert await saver.aget(config) == cp
        all_checks = await saver.alist(config)
        assert len(all_checks) == 1

    asyncio.run(_run())
    saver.close()


def test_crewai_memory_save_search_reset(tmp_aikoql):
    mem = AikoqlCrewAIMemory.from_client(tmp_aikoql, role="researcher")

    mem.save("cats are mammals", {"source": "biology"})
    mem.save("dogs are mammals", {"source": "biology"})
    mem.save("fish live underwater", {"source": "biology"})

    hits = mem.search("pets", limit=2)
    assert len(hits) == 2
    assert all("mammals" in h for h in hits)

    mem.reset()
    assert mem.search("pets", limit=5) == []

    mem.close()


def test_legacy_checkpointer_alias(tmp_aikoql):
    # The old import path must keep working.
    from aikoql.checkpointer import AikoqlCheckpointer

    cp = AikoqlCheckpointer.from_client(tmp_aikoql)
    config = {"configurable": {"thread_id": "legacy"}}
    cp.put(config, {"id": "c1", "ts": "1", "channel_values": {}})
    assert cp.get(config)["id"] == "c1"
    cp.close()
