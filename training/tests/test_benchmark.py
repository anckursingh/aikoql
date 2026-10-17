"""T-13 RED: scripts/benchmark_dataset.py (design §27 cells).

The benchmark runs the full generate pipeline against a live server
and reports three cells as JSON: throughput (wall seconds,
examples, examples_per_second), rates (the pipeline's derived
rates) and size (dataset bytes on disk, per-split counts). Laptop
scale: the 2-service fixture end to end — the corpus-scale cell
arrives with T-14's dataset.

Every test below fails against the current tree:
`training/scripts/benchmark_dataset.py` does not exist.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

_SCRIPT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "scripts", "benchmark_dataset.py")
_SRC = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src")


def test_benchmark_reports_all_three_cells(mcp_server, tmp_path):
    host, token = mcp_server
    out = tmp_path / "dataset"
    env = dict(os.environ)
    env["PYTHONPATH"] = _SRC + os.pathsep + env.get("PYTHONPATH", "")
    proc = subprocess.run(
        [sys.executable, _SCRIPT, "--db", host, "--token", token,
         "--out", str(out)],
        env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    cells = json.loads(proc.stdout)
    assert cells["throughput"]["wall_s"] > 0
    assert cells["throughput"]["examples_per_second"] > 0
    assert cells["rates"]["refusal"] is not None
    assert cells["size"]["bytes"] > 0
    assert sum(cells["size"]["splits"].values()) == \
        cells["throughput"]["examples"]
