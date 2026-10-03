"""T-14 mutation leg: kill every registered validator mutant.

Each mutant is one string surgery on the E1-E9 eval-set validator
(training/src/aikoql_training/validation/eval_set.py), applied to a
fresh copy of training/src per mutant (count-1 replace — a mutant is
registered only where its anchor is unique). A mutant is KILLED iff
the eval-set suite fails against it (rc != 0). The leg exits 0 iff at
least one mutant is registered and every mutant died.

stdout: one JSON line {mutants, killed, survivors}.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

_MUTANTS = [
    # E1: the exact-fact check becomes fiction
    ('if task == "factual":', 'if task == "fictional":', 1),
    # E2: membership inverted
    ("if ent not in mentions:", "if ent in mentions:", 1),
    # E3: the query-head regex accepts anything
    (r'_HEAD = re.compile(r"^\s*(?:MATCH|TRAVERSE)\b")',
     r'_HEAD = re.compile(r"^")', 1),
    # E5: the temporal check looks for a misspelling
    ('"AS_OF" not in query', '"ASOF" not in query', 1),
    # E7: unknown examples become answerable
    ('if labels.get("answerable") is not False:',
     'if labels.get("answerable") is not None:', 1),
    # E8: the policy-decision prefix loosened
    ('_DECISION = "Policy decision: "', '_DECISION = "Policy: "', 1),
    # E9: the contradiction claim-koid pattern weakened
    (r"[0-9a-f]{8}", r"[0-9]{8}", 1),
]

_REPO = Path(__file__).resolve().parents[2]
_TESTS = "training/tests/test_eval_set.py"


def _mutant_tree(workdir: Path, i: int, old: str, new: str, count: int) -> Path:
    """A fresh copy of training/src with mutant i applied."""
    src = workdir / f"mutant-{i}" / "src"
    shutil.rmtree(src, ignore_errors=True)  # a rerun must rebuild the tree
    shutil.copytree(_REPO / "training" / "src", src,
                    ignore=shutil.ignore_patterns("__pycache__"))
    target = src / "aikoql_training" / "validation" / "eval_set.py"
    text = target.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"mutant {i} anchor gone: {old!r}")
    target.write_text(text.replace(old, new, count), encoding="utf-8")
    return src


def _killed(src: Path) -> bool:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(src) + os.pathsep + env.get("PYTHONPATH", "")
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", str(_REPO / _TESTS), "-q",
         # training/pyproject.toml sets pythonpath=["src"], which pytest
         # force-inserts at sys.path[0] ahead of this PYTHONPATH — clear
         # it or every mutant tree runs the unmutated validator.
         "-o", "pythonpath="],
        cwd=_REPO, env=env, capture_output=True, text=True,
        timeout=600,
    )
    return proc.returncode != 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--workdir", required=True,
                        help="scratch directory for the mutant trees")
    args = parser.parse_args(argv)

    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    killed, survivors = [], []
    for i, (old, new, count) in enumerate(_MUTANTS):
        src = _mutant_tree(workdir, i, old, new, count)
        if _killed(src):
            killed.append(old)
        else:
            survivors.append(old)
    print(json.dumps({"mutants": len(_MUTANTS), "killed": len(killed),
                      "survivors": survivors}, sort_keys=True))
    return 0 if _MUTANTS and not survivors else 1


if __name__ == "__main__":
    sys.exit(main())
