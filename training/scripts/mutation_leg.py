"""T-14 mutation leg: kill every registered validator mutant.

Each mutant is one string surgery on a validator source file (path
relative to training/src/aikoql_training), applied to a fresh copy of
training/src per mutant (count-1 replace — a mutant is registered only
where its anchor is unique). A mutant is KILLED iff its test file fails
against it (rc != 0). The leg exits 0 iff at least one mutant is
registered and every mutant died.

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

# (file, old, new, count, tests) — file is relative to
# src/aikoql_training; tests is the suite that must kill the mutant.
_MUTANTS = [
    # E1: the exact-fact check becomes fiction
    ("validation/eval_set.py",
     'if task == "factual":', 'if task == "fictional":', 1,
     "training/tests/test_eval_set.py"),
    # E2: membership inverted
    ("validation/eval_set.py",
     "if ent not in mentions:", "if ent in mentions:", 1,
     "training/tests/test_eval_set.py"),
    # E3: the query-head regex accepts anything
    ("validation/eval_set.py",
     r'_HEAD = re.compile(r"^\s*(?:MATCH|TRAVERSE)\b")',
     r'_HEAD = re.compile(r"^")', 1,
     "training/tests/test_eval_set.py"),
    # E5: the temporal check looks for a misspelling
    ("validation/eval_set.py",
     '"AS_OF" not in query', '"ASOF" not in query', 1,
     "training/tests/test_eval_set.py"),
    # E7: unknown examples become answerable
    ("validation/eval_set.py",
     'if labels.get("answerable") is not False:',
     'if labels.get("answerable") is not None:', 1,
     "training/tests/test_eval_set.py"),
    # E8: the policy-decision prefix loosened
    ("validation/eval_set.py",
     '_DECISION = "Policy decision: "', '_DECISION = "Policy: "', 1,
     "training/tests/test_eval_set.py"),
    # E9: the contradiction claim-koid pattern weakened
    ("validation/eval_set.py",
     r"[0-9a-f]{8}", r"[0-9]{8}", 1,
     "training/tests/test_eval_set.py"),
    # T-25 (PR9 §26): authorization mutants — always allow (the denial
    # branch dies in the grounding validator), ignore subject, ignore
    # action, ignore resource (each dies in the schema demand loop).
    ("validation/grounding.py",
     'denied = answer.startswith("DENIED:")', 'denied = False', 1,
     "training/tests/test_authorization.py"),
    ("models.py",
     'for key in ("subject", "action", "resource"):',
     'for key in ("action", "resource"):', 1,
     "training/tests/test_schema.py"),
    ("models.py",
     'for key in ("subject", "action", "resource"):',
     'for key in ("subject", "resource"):', 1,
     "training/tests/test_schema.py"),
    ("models.py",
     'for key in ("subject", "action", "resource"):',
     'for key in ("subject", "action"):', 1,
     "training/tests/test_schema.py"),
    # T-28 (PR9 §26): the holdout force — an example whose org is held
    # out hashes into val/test only (disarmed, a held-out example
    # hashes into train and the tamper is undetectable), and the gate's
    # manifest declaration — ignored, the recompute can never see the
    # holdout. Both die in test_gates.py.
    ("dataset/splitter.py",
     'if ex.get("org") in held_out:', 'if False:', 1,
     "training/tests/test_gates.py"),
    ("dataset/gates.py",
     'tuple(manifest.get("held_out_orgs") or ())', '()', 1,
     "training/tests/test_gates.py"),
]

_REPO = Path(__file__).resolve().parents[2]


def _mutant_tree(workdir: Path, i: int, file: str, old: str, new: str,
                 count: int) -> Path:
    """A fresh copy of training/src with mutant i applied."""
    src = workdir / f"mutant-{i}" / "src"
    shutil.rmtree(src, ignore_errors=True)  # a rerun must rebuild the tree
    shutil.copytree(_REPO / "training" / "src", src,
                    ignore=shutil.ignore_patterns("__pycache__"))
    target = src / "aikoql_training" / file
    text = target.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"mutant {i} anchor gone: {old!r}")
    target.write_text(text.replace(old, new, count), encoding="utf-8")
    return src


def _killed(src: Path, tests: str) -> bool:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(src) + os.pathsep + env.get("PYTHONPATH", "")
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", str(_REPO / tests), "-q",
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
    for i, (file, old, new, count, tests) in enumerate(_MUTANTS):
        src = _mutant_tree(workdir, i, file, old, new, count)
        if _killed(src, tests):
            killed.append(old)
        else:
            survivors.append(old)
    print(json.dumps({"mutants": len(_MUTANTS), "killed": len(killed),
                      "survivors": survivors}, sort_keys=True))
    return 0 if _MUTANTS and not survivors else 1


if __name__ == "__main__":
    sys.exit(main())
