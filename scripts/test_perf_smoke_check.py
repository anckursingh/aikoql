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
CELLS = {"write_per_sync": 1.2}  # baseline: 1.2 syncs' worth of time per commit


def rows(write_ns, sync_ns):
    out = {(BACKEND, "ingestion (W6)"): float(write_ns)}
    if sync_ns is not None:
        out[(BACKEND, "machine sync (ref)")] = float(sync_ns)
    return out


class WriteCellTest(unittest.TestCase):
    def test_a_slow_machine_passes(self):
        # 37e2099's runner: write 7.2 ms — but its syncs were slow too
        ratio = psc.check_write(rows(7_218_326, 6_000_000), CELLS)
        self.assertLess(ratio, psc.BOUND)

    def test_an_extra_sync_per_commit_fails(self):
        # the same machine, the write path now costs 4.8 syncs per commit
        with self.assertRaises(psc.PerfFail) as e:
            psc.check_write(rows(7_200_000, 1_500_000), CELLS)
        self.assertIn("ingestion (W6)", str(e.exception))

    def test_a_missing_machine_reference_fails_by_name(self):
        with self.assertRaises(psc.PerfFail) as e:
            psc.check_write(rows(7_200_000, None), CELLS)
        self.assertIn("machine sync (ref)", str(e.exception))


if __name__ == "__main__":
    unittest.main()
