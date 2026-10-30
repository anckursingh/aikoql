#!/usr/bin/env python3
"""The perf smoke's write cell is judged against the machine it ran on.

The ingestion (W6) cell is the mean commit wall per put — sync-bound, so on
a shared runner it tracks that machine's disk, not the code: identical code
measured 4.4x-9.7x the laptop baseline across CI runners (37e2099: the
whole write suite ran 2x slower while the reads stayed at 0.6x). The gate
now divides it by a plain sync measured on the same disk in the same run
("machine sync (ref)"): a slow machine scales both and passes; an extra sync
per commit, or super-linear work, moves only the numerator and fails.

Run: python scripts/test_perf_smoke_check.py
"""
import importlib.util
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
_spec = importlib.util.spec_from_file_location("perf_smoke_check", HERE / "perf-smoke-check.py")
psc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(psc)

BACKEND = "aikoql-v2"
CELLS = {"write_per_sync": 1.0}  # baseline: a commit costs one sync (measured 1.00-1.04)


def rows(write_ns, sync_ns):
    out = {(BACKEND, "ingestion (W6)"): float(write_ns)}
    if sync_ns is not None:
        out[(BACKEND, "machine sync (ref)")] = float(sync_ns)
    return out


class WriteCellTest(unittest.TestCase):
    def test_a_slow_machine_passes(self):
        # 37e2099's runner: write 7.2 ms (9.66x the laptop) — its syncs
        # were as slow, so the commit still costs about one sync
        ratio = psc.check_write(rows(7_218_326, 7_000_000), CELLS)
        self.assertLess(ratio, psc.SYNC_BOUND)

    def test_run_to_run_noise_passes(self):
        # the spread measured on one machine: 1.00-1.04 syncs per commit
        psc.check_write(rows(3_150_651, 3_026_125), CELLS)

    def test_one_extra_sync_per_commit_fails(self):
        # the same machine, the write path now syncs twice per commit
        with self.assertRaises(psc.PerfFail) as e:
            psc.check_write(rows(6_000_000, 3_000_000), CELLS)
        self.assertIn("ingestion (W6)", str(e.exception))

    def test_a_missing_machine_reference_fails_by_name(self):
        with self.assertRaises(psc.PerfFail) as e:
            psc.check_write(rows(7_200_000, None), CELLS)
        self.assertIn("machine sync (ref)", str(e.exception))


# The write_per_sync measured where the gate runs — Windows CI runners,
# run 40937044377 (1effc8c), attempts 1-3: 1.36, 1.35 (westus), 1.20 (eastus).
# (macOS measured 1.00-1.04: a Windows commit costs ~0.3 syncs more.)
WINDOWS_SAMPLES = (1.36, 1.35, 1.20)


class CommittedBaselineTest(unittest.TestCase):
    def setUp(self):
        import json

        base = json.loads((HERE.parent / psc.BASE).read_text(encoding="utf-8"))
        self.cells = base["cells"]

    def test_the_measured_windows_spread_keeps_its_headroom(self):
        # the noise must sit well inside the budget, not at its edge
        for per_sync in WINDOWS_SAMPLES:
            ratio = psc.check_write(rows(per_sync * 1e6, 1e6), self.cells)
            self.assertLess(ratio, 0.8 * psc.SYNC_BOUND, f"sample {per_sync}")

    def test_one_extra_sync_over_the_windows_median_fails(self):
        median = sorted(WINDOWS_SAMPLES)[1]
        with self.assertRaises(psc.PerfFail):
            psc.check_write(rows((median + 1.0) * 1e6, 1e6), self.cells)


class CompactWallTest(unittest.TestCase):
    """The compaction cell is the median of several merges: c2af329's perf
    run failed on ONE stalled merge (361 ms; the same code reads 49-102 ms,
    the allocations were exactly the baseline's) — a single sample let a
    one-off stall (the runner, not the merge) fail the gate."""

    def test_one_stalled_merge_does_not_fail_the_cell(self):
        wall = psc.compact_wall_ms({"compact_wall_samples": [64, 361, 71, 58, 77]})
        self.assertEqual(wall, 71)

    def test_consistently_slow_merges_still_fail(self):
        wall = psc.compact_wall_ms({"compact_wall_samples": [340, 361, 355, 349, 370]})
        self.assertGreater(wall / 37.0, psc.FSYNC_BOUND)

    def test_too_few_samples_fail_by_name(self):
        for cells in ({}, {"compact_wall_samples": [64]}, {"compact_wall_samples": [64, 70]}):
            with self.assertRaises(psc.PerfFail) as e:
                psc.compact_wall_ms(cells)
            self.assertIn("compact_wall_samples", str(e.exception))


if __name__ == "__main__":
    unittest.main()
