"""T-14 RED: AcmePay POC corpus (design Phase 17, §35–37).

The corpus generator is the dataset engine's product: seed the
AcmePay KB (~100 entities, §35 counts) into a live server, run every
scenario family through the oracle, and emit a dataset that hits the
example target through a deterministic seed sweep. The contract:

- determinism-across-seeds: the same sweep seeds regenerate the same
  question multiset and task-type histogram on ANY server (koid-free
  identity — HLC koids are mint-fresh per run);
- scale: the sweep keeps seeding until the target is reached;
- leakage: the published corpus has zero cross-holdout pairs;
- security: a secret planted in the corpus fails the gate and the
  pipeline refuses to publish;
- artifacts: manifest + statistics committed under the artifacts dir.

Every test below fails against the current tree:
`training/scripts/generate_corpus.py` does not exist.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

from aikoql_training.dataset.writer import read_dataset

_ROOT = Path(__file__).parents[2]  # training/tests -> repo root
_SRC = _ROOT / "training" / "src"
_CORPUS = _ROOT / "training" / "scripts" / "generate_corpus.py"


def _run(host, token, out, target, seeds, artifacts, extra=None):
    env = dict(os.environ)
    env["PYTHONPATH"] = str(_SRC) + os.pathsep + env.get("PYTHONPATH", "")
    cmd = [sys.executable, str(_CORPUS), "--db", host, "--token", token,
           "--out", str(out), "--target", str(target), "--seeds", str(seeds),
           "--artifacts", str(artifacts)] + (extra or [])
    return subprocess.run(cmd, env=env, capture_output=True, text=True,
                          timeout=600)


def _report(proc):
    return json.loads(proc.stdout)


def test_corpus_script_exists():
    assert _CORPUS.exists(), "training/scripts/generate_corpus.py missing"


def test_corpus_reaches_the_target_and_is_publishable(mcp_server, tmp_path):
    host, token = mcp_server
    out, artifacts = tmp_path / "ds", tmp_path / "artifacts"
    proc = _run(host, token, out, target=40, seeds=2, artifacts=artifacts)
    assert proc.returncode == 0, proc.stderr
    report = _report(proc)
    assert report["publishable"] is True, report
    assert report["example_count"] >= 40, report
    # the leakage gate holds at corpus scale: zero cross-holdout pairs
    assert report["gates"]["leakage"]["count"] == 0, report
    # the corpus-time secret binding holds: secret_scan evaluated and 0
    assert report["gates"]["secret_scan"]["ok"] is True, report


def test_corpus_commits_manifest_and_statistics(tmp_path, mcp_server):
    host, token = mcp_server
    out, artifacts = tmp_path / "ds", tmp_path / "artifacts"
    proc = _run(host, token, out, target=30, seeds=1, artifacts=artifacts)
    assert proc.returncode == 0, proc.stderr
    manifest = json.loads((artifacts / "manifest.json").read_text(
        encoding="utf-8"))
    stats = json.loads((artifacts / "stats.json").read_text(
        encoding="utf-8"))
    assert manifest["example_count"] >= 30
    assert manifest["seeds"] == [0]
    assert stats["seeds"] == 1
    assert sum(stats["per_seed"].values()) == manifest["example_count"]


def test_corpus_regenerates_identically_across_servers(tmp_path):
    """Determinism-across-seeds, koid-agnostic: fresh HLC koids per
    server, so the identity is the question multiset + task-type
    histogram — the sweep seeds drive the picks, not the clock."""
    from conftest import _serve  # two independent servers

    def one(host, token, out):
        artifacts = out / "artifacts"
        proc = _run(host, token, out, target=40, seeds=2,
                    artifacts=artifacts)
        assert proc.returncode == 0, proc.stderr
        ds = read_dataset(str(out))
        examples = [e for n in ("train", "val", "test") for e in ds[n]]
        questions = sorted(e["input"]["question"] for e in examples)
        histogram = sorted((t, sum(1 for e in examples
                                   if e["task"]["type"] == t))
                           for t in {e["task"]["type"] for e in examples})
        return questions, histogram

    for host, _specs in _serve(["test-token::admin"]):
        first = one(host, "test-token", tmp_path / "a")
        break
    for host, _specs in _serve(["test-token::admin"]):
        second = one(host, "test-token", tmp_path / "b")
        break
    assert first == second


def test_corpus_security_refuses_a_planted_secret(mcp_server, tmp_path):
    """Security is fail-closed: a secret in the corpus fails the gate
    and the validator refuses to publish it (exit 1)."""
    host, token = mcp_server
    out, artifacts = tmp_path / "ds", tmp_path / "artifacts"
    proc = _run(host, token, out, target=30, seeds=1, artifacts=artifacts)
    assert proc.returncode == 0, proc.stderr

    # plant a secret into a train example's context fact
    train = out / "train.jsonl"
    lines = train.read_text(encoding="utf-8").splitlines()
    example = json.loads(lines[0])
    example["context"]["facts"][0]["statement"] = (
        "The token is sk-live-9f8b7a6c5d4e3f2a1b0c9d8e")
    example["example_id"] = "f" * 64
    train.write_text("\n".join([json.dumps(example)] + lines[1:]),
                     encoding="utf-8")

    env = dict(os.environ)
    env["PYTHONPATH"] = str(_SRC) + os.pathsep + env.get("PYTHONPATH", "")
    check = subprocess.run(
        [sys.executable, "-m", "aikoql_training.cli", "validate",
         str(out), "--db", host, "--token", token],
        env=env, capture_output=True, text=True, timeout=300)
    assert check.returncode == 1
    report = json.loads(check.stdout)
    assert report["publishable"] is False
    assert report["gates"]["secret_scan"]["ok"] is False
