"""T-14 RED: the mutation leg (design §37) — validator/eval mutants
must ALL be killed by the eval-set tests.

The leg copies the src tree, applies each registered mutant (a
single semantic sabotage of the validator or eval-set logic), runs
the eval tests against the mutant tree, and reports the survivors.
Exit 0 iff zero survive: a mutant the tests cannot see is a gate
with no teeth, and a toothless gate must fail the milestone.

Every test below fails against the current tree:
`training/scripts/mutation_leg.py` does not exist.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

_ROOT = Path(__file__).parents[2]  # training/tests -> repo root
_SRC = _ROOT / "training" / "src"
_LEG = _ROOT / "training" / "scripts" / "mutation_leg.py"


def _run(tmp_path, extra=None):
    env = dict(os.environ)
    env["PYTHONPATH"] = str(_SRC) + os.pathsep + env.get("PYTHONPATH", "")
    cmd = [sys.executable, str(_LEG), "--workdir", str(tmp_path)] + \
        (extra or [])
    return subprocess.run(cmd, env=env, capture_output=True, text=True,
                          timeout=900)


def test_mutation_leg_script_exists():
    assert _LEG.exists(), "training/scripts/mutation_leg.py missing"


def test_mutation_leg_kills_every_mutant(tmp_path):
    """At least one mutant registered, and every one of them killed."""
    proc = _run(tmp_path)
    assert proc.returncode == 0, proc.stderr
    report = json.loads(proc.stdout)
    assert report["mutants"] >= 1, report
    assert report["killed"] == report["mutants"], report
    assert report["survivors"] == [], report
