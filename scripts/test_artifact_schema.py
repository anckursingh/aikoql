#!/usr/bin/env python3
"""P5-M47 (R4-P2-04/05) — fixture pins for the checker-script schema validation.

The two perf gate checkers (gate5-check.py, perf-smoke-check.py) read the
harness artifacts field-by-field; a schema drift anywhere raises a raw
KeyError with no breadcrumb. These pins fix the contract of the shared
validator (artifact_schema.py):

  * every consumed field is validated up front with a NAMED error —
    "missing field: p50_ns" plus the exact row, never a KeyError;
  * the fresh artifact's environment.git_sha must equal the tested HEAD
    (git rev-parse HEAD) — stale evidence can never silently feed the
    gate; the committed 75391b82 artifact is the live instance of this
    RED (stale against every head since c19cee5);
  * the __main__ entry (python scripts/artifact_schema.py <path>) is the
    freshness gate the CI republish job stamps its uploads with.

Run: python scripts/test_artifact_schema.py
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import artifact_schema
from artifact_schema import SchemaError

HERE = Path(__file__).resolve().parent


def write_json(name, obj):
    d = Path(tempfile.mkdtemp(prefix="m47-schema-"))
    p = d / name
    p.write_text(json.dumps(obj), encoding="utf-8")
    return p


def row(label="KO get (W1)", p50_ns=58900):
    return {"label": label, "ops": 200, "p50_ns": p50_ns, "p99_ns": 1}


def one_m(**kw):
    """A minimal valid 1M artifact, overridable per pin."""
    o = {
        "environment": {"git_sha": kw.get("sha", artifact_schema.head_sha())},
        "backends": [
            {
                "name": "aikoql-v2",
                "rows": kw.get("rows", [row(), row("head get (W2)", 41800)]),
            }
        ],
    }
    return write_json("result-1m.json", o)


def workload_cell(name, n=50):
    return {"name": name, "n": n, "p50_ms": 13.7, "p95_ms": 21.0,
            "p99_ms": 30.0, "throughput_ops_s": 71.0, "correct": True}


def one_competitor(**kw):
    """A minimal valid competitor (§13) artifact, overridable per pin."""
    engines = {}
    for key in artifact_schema.COMPETITOR_ENGINES:
        engines[key] = {
            "workloads": [workload_cell(w)
                          for w in artifact_schema.COMPETITOR_WORKLOADS],
            "rss_kb": 12345, "memory_mb": 12, "disk_bytes": 123456,
            "cpu_seconds": 4.5, "ingest_s": 2.5,
        }
    o = {
        "commit": "abc1234",
        "seed": 42,
        "config": {"n_notes": 1000, "n_events": 500, "n_per_cell": 50,
                   "warmup_ops": 10, "seed": 42},
        "dataset": {"notes": 1000, "events": 500},
        "environment": {"os": "Windows", "cpu": "x64", "ram_mb": 32768,
                        "cache_state": "warm",
                        "harness_sha": kw.get("sha",
                                              artifact_schema.head_sha()),
                        "git_sha": kw.get("sha",
                                          artifact_schema.head_sha())},
        "engine_versions": {
            "aikoql": "0.1.19",
            "pg": {"image": "pgvector/pgvector:pg16", "digest": "sha256:a"},
            "neo4j": {"image": "neo4j:5-community", "digest": "sha256:a"},
            "qdrant": {"image": "qdrant/qdrant:v1.19.1", "digest": "sha256:a"},
            "mongo": {"image": "mongo:7", "digest": "sha256:a"},
        },
        "engines": engines,
    }
    return write_json("result-competitor.json", o)


class SchemaPins(unittest.TestCase):
    def test_missing_p50_ns_names_the_field_and_row(self):
        bad = row()
        del bad["p50_ns"]
        p = one_m(rows=[bad])
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=False)
        msg = str(cm.exception)
        self.assertIn("missing field: p50_ns", msg)
        self.assertIn("KO get (W1)", msg)
        self.assertIn(str(p), msg)

    def test_p50_ns_wrong_type_is_named_not_a_number(self):
        p = one_m(rows=[row(p50_ns="58900")])
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=False)
        self.assertIn("not a number", str(cm.exception))

    def test_missing_backends_names_the_field(self):
        o = {
            "environment": {"git_sha": artifact_schema.head_sha()},
        }
        p = write_json("result-1m.json", o)
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=False)
        self.assertIn("missing field: backends", str(cm.exception))

    def test_fresh_requires_git_sha(self):
        p = one_m(sha="")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=True)
        self.assertIn("missing field: environment.git_sha", str(cm.exception))

    def test_stale_artifact_names_both_shas(self):
        p = one_m(sha="75391b82c8189f7b26e15e5cf64e775d4c4c1edd")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=True)
        msg = str(cm.exception)
        self.assertIn("stale artifact", msg)
        self.assertIn("75391b82c8189f7b26e15e5cf64e775d4c4c1edd", msg)
        self.assertIn(artifact_schema.head_sha(), msg)

    def test_fresh_stamp_at_head_passes(self):
        p = one_m()
        rows = artifact_schema.validate_1m(str(p), fresh=True)
        self.assertEqual(rows[("aikoql-v2", "KO get (W1)")], 58900.0)

    def test_valid_1m_returns_labeled_rows(self):
        p = one_m()
        rows = artifact_schema.validate_1m(str(p), fresh=False)
        self.assertEqual(
            sorted(rows),
            [("aikoql-v2", "KO get (W1)"), ("aikoql-v2", "head get (W2)")],
        )

    def test_smoke_rows_missing_label_is_named(self):
        bad = row()
        del bad["label"]
        p = one_m(rows=[bad])
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(p), fresh=False)
        self.assertIn("missing field: label", str(cm.exception))

    def test_smoke_baseline_cells_missing_key_is_named(self):
        o = {
            "cells": {
                "w1_ko_get_p50_ns": 6600.0,
                "w2_head_get_p50_ns": 6300.0,
                "write_p50_ns": 4100.0,
                "scan_p50_ns": 900.0,
                "hot_head_p50_ns": 1300.0,
                "compact_wall_ms": 12.0,
            }
        }
        p = write_json("perf-smoke-baseline.json", o)
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_smoke_cells(str(p))
        self.assertIn("missing field: compact_allocs", str(cm.exception))

    def test_smoke_baseline_cells_valid(self):
        o = {
            "cells": {
                "w1_ko_get_p50_ns": 6600.0,
                "w2_head_get_p50_ns": 6300.0,
                "write_p50_ns": 4100.0,
                "scan_p50_ns": 900.0,
                "hot_head_p50_ns": 1300.0,
                "compact_wall_ms": 12.0,
                "compact_allocs": 500.0,
            }
        }
        p = write_json("perf-smoke-baseline.json", o)
        cells = artifact_schema.validate_smoke_cells(str(p))
        self.assertEqual(cells["compact_allocs"], 500.0)

    def test_unreadable_file_flags_for_the_hint(self):
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_1m(str(HERE / "does-not-exist.json"))
        self.assertTrue(cm.exception.unreadable)

    def test_main_stale_exits_nonzero(self):
        p = one_m(sha="deadbeef")
        r = subprocess.run(
            [sys.executable, str(HERE / "artifact_schema.py"), str(p)],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("stale artifact", r.stderr)

    def test_main_fresh_exits_zero(self):
        p = one_m()
        r = subprocess.run(
            [sys.executable, str(HERE / "artifact_schema.py"), str(p)],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)

    # CI-08 (§13/§18): the competitor artifact contract

    def test_competitor_valid_returns_all_matrix_rows(self):
        p = one_competitor()
        rows = artifact_schema.validate_competitor(str(p), fresh=False)
        self.assertEqual(len(rows), 5 * 7)
        self.assertEqual(rows[("aikoql", "point_read")], 13.7)

    def test_competitor_missing_cpu_seconds_is_named(self):
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        del o["engines"]["postgresql"]["cpu_seconds"]
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_competitor(str(p), fresh=False)
        msg = str(cm.exception)
        self.assertIn("engines[postgresql]", msg)
        self.assertIn("missing field: cpu_seconds", msg)

    def test_competitor_aikoql_unmeasured_cpu_is_named(self):
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        o["engines"]["aikoql"]["cpu_seconds"] = None
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_competitor(str(p), fresh=False)
        self.assertIn("measured in-process", str(cm.exception))

    def test_competitor_container_metrics_may_degrade_to_none(self):
        # CI runners cannot probe their service containers — None is legal
        # for the container columns, never for aikoql.
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        for key in ("postgresql", "neo4j", "qdrant", "mongodb"):
            o["engines"][key].update(cpu_seconds=None, memory_mb=None,
                                     disk_bytes=None)
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        rows = artifact_schema.validate_competitor(str(p), fresh=False)
        self.assertEqual(len(rows), 5 * 7)

    def test_competitor_missing_digest_is_named(self):
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        del o["engine_versions"]["qdrant"]["digest"]
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_competitor(str(p), fresh=False)
        msg = str(cm.exception)
        self.assertIn("engine_versions.qdrant", msg)
        self.assertIn("image + digest", msg)

    def test_competitor_null_engine_versions_allowed(self):
        # the docker-probe-less runner shape (§18 enforced by the arch gate)
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        for key in ("pg", "neo4j", "qdrant", "mongo"):
            o["engine_versions"][key] = None
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        rows = artifact_schema.validate_competitor(str(p), fresh=False)
        self.assertEqual(len(rows), 5 * 7)

    def test_competitor_missing_environment_field_is_named(self):
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        del o["environment"]["harness_sha"]
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_competitor(str(p), fresh=False)
        self.assertIn("environment.harness_sha", str(cm.exception))

    def test_competitor_cell_missing_p99_is_named(self):
        p = one_competitor()
        o = json.loads(Path(p).read_text(encoding="utf-8"))
        del o["engines"]["neo4j"]["workloads"][6]["p99_ms"]
        Path(p).write_text(json.dumps(o), encoding="utf-8")
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_competitor(str(p), fresh=False)
        msg = str(cm.exception)
        self.assertIn("knowledge_query", msg)
        self.assertIn("missing field: p99_ms", msg)

    def test_main_dispatch_competitor_fresh_exits_zero(self):
        p = one_competitor()
        r = subprocess.run(
            [sys.executable, str(HERE / "artifact_schema.py"), str(p)],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("35 validated rows", r.stdout)

    def test_main_dispatch_competitor_stale_exits_nonzero(self):
        p = one_competitor(sha="75391b82c8189f7b26e15e5cf64e775d4c4c1edd")
        r = subprocess.run(
            [sys.executable, str(HERE / "artifact_schema.py"), str(p)],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("stale artifact", r.stderr)


if __name__ == "__main__":
    os.chdir(HERE)  # git rev-parse must resolve against the repo
    unittest.main(verbosity=2)
