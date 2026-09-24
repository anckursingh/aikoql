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
            }
        }
        p = write_json("perf-smoke-baseline.json", o)
        with self.assertRaises(SchemaError) as cm:
            artifact_schema.validate_smoke_cells(str(p))
        self.assertIn("missing field: hot_head_p50_ns", str(cm.exception))

    def test_smoke_baseline_cells_valid(self):
        o = {
            "cells": {
                "w1_ko_get_p50_ns": 6600.0,
                "w2_head_get_p50_ns": 6300.0,
                "hot_head_p50_ns": 1300.0,
            }
        }
        p = write_json("perf-smoke-baseline.json", o)
        cells = artifact_schema.validate_smoke_cells(str(p))
        self.assertEqual(cells["hot_head_p50_ns"], 1300.0)

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


if __name__ == "__main__":
    os.chdir(HERE)  # git rev-parse must resolve against the repo
    unittest.main(verbosity=2)
