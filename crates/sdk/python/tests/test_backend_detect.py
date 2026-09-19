"""PR6-005 — one authoritative backend decision path (docs/STORAGE-BACKENDS.md).

The review's contract: an explicit backend always wins; with NO explicit
backend the existing on-disk format is detected (redb file -> redb, native
AKQL WAL -> aikoql, v2 directory -> aikoql-v2), and only a MISSING path
defaults to a fresh aikoql-v2. The SDK must route through that shared
decision path — its own hardcoded default would be a second, divergent
implicit rule (an existing redb database must keep opening and serve its
data, never be re-opened as a fresh v2 store).
"""

import gc
import os
import shutil
import tempfile

import pytest

from aikoql import aikoql


@pytest.fixture
def tmp_path_only():
    d = tempfile.mkdtemp(prefix="aikoql-py-backend-")
    yield os.path.join(d, "test.redb")
    shutil.rmtree(d, ignore_errors=True)


def test_no_backend_autodetects_existing_redb(tmp_path_only):
    path = tmp_path_only
    db = aikoql(path, salt=42, backend="redb")
    r = db.remember("alice", "fact", {"body": "pre-flip data"})
    db.close()
    # redb holds an exclusive process-wide file lock; close() is a no-op in
    # this revision (drop-on-GC), so release the handle deterministically.
    del db
    gc.collect()

    reopened = aikoql(path, salt=42)  # no explicit backend: detection must win
    try:
        ko = reopened.get("alice", r["koid"])
        assert ko["properties"]["body"] == "pre-flip data"
    finally:
        reopened.close()


def test_no_backend_autodetects_existing_v1(tmp_path_only):
    path = tmp_path_only
    db = aikoql(path, salt=42, backend="aikoql")
    r = db.remember("alice", "fact", {"body": "native wal data"})
    db.close()
    del db
    gc.collect()

    reopened = aikoql(path, salt=42)  # no explicit backend: detection must win
    try:
        ko = reopened.get("alice", r["koid"])
        assert ko["properties"]["body"] == "native wal data"
    finally:
        reopened.close()
