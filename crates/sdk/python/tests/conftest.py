"""Shared test plumbing: temp db dirs removed at teardown, corpses purged at startup.

Killed runs never run teardown, and close() is a documented no-op in this
revision (drop-on-GC) — so the fixtures here `del` the client and collect
before rmtree (redb holds an exclusive file lock until the Rust struct
drops), and any aikoql-py-* temp dir older than a day is swept at session
start so the next run collects what a dead run left behind.
"""

import gc
import os
import shutil
import tempfile
import time

import pytest

_PREFIX = "aikoql-py-"
_STALE_S = 24 * 3600


def _purge_stale() -> None:
    d = tempfile.gettempdir()
    cutoff = time.time() - _STALE_S
    try:
        entries = os.listdir(d)
    except OSError:
        return
    for name in entries:
        if not name.startswith(_PREFIX):
            continue
        p = os.path.join(d, name)
        try:
            if os.path.getmtime(p) < cutoff:
                if os.path.isdir(p):
                    shutil.rmtree(p, ignore_errors=True)
                else:
                    os.unlink(p)
        except OSError:
            continue


_purge_stale()


@pytest.fixture
def tmp_aikoql():
    from aikoql import aikoql

    d = tempfile.mkdtemp(prefix=_PREFIX)
    path = os.path.join(d, "test.redb")
    client = aikoql(path, salt=42)
    try:
        yield client
    finally:
        client.close()  # no-op this revision — release happens on drop
        del client
        gc.collect()  # drop the Rust struct so redb's file lock releases
        shutil.rmtree(d, ignore_errors=True)
