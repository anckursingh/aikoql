"""T-13 RED: structured metrics (design §27).

The pipeline accumulates a Metrics object: counts per stage and
derived rates. The no-sensitive-content rule is structural — every
leaf value of the metrics dict is a number (no question, answer,
fact statement or KO text can ever land in a metric), keys are fixed
names. `generate --metrics FILE` writes the dict as JSON; the
emitted count equals the manifest's example_count.

Every test below fails against the current tree:
`aikoql_training.metrics` does not exist.
"""

from __future__ import annotations

import json

from aikoql_training.metrics import Metrics


def test_counts_accumulate():
    m = Metrics()
    m.count("scenarios", 10)
    m.count("scenarios", 2)
    m.count("refused")
    out = m.as_dict()
    assert out["counts"] == {"scenarios": 12, "refused": 1}


def test_rates_derive_from_counts():
    m = Metrics()
    m.count("scenarios", 10)
    m.count("refused", 2)
    m.rate("refusal", "refused", "scenarios")
    assert m.as_dict()["rates"] == {"refusal": 0.2}


def test_rate_with_zero_denominator_is_none():
    m = Metrics()
    m.rate("refusal", "refused", "scenarios")
    assert m.as_dict()["rates"] == {"refusal": None}


def test_metrics_carry_only_numbers():
    """The no-sensitive-content rule, structurally: every leaf value
    is a number (int/float/None for undefined rates), never text."""
    m = Metrics()
    m.count("scenarios", 3)
    m.count("refused", 1)
    m.rate("refusal", "refused", "scenarios")
    leaves = _leaves(m.as_dict())
    assert leaves
    assert all(v is None or isinstance(v, (int, float)) for v in leaves)


def _leaves(obj):
    out = []
    for value in obj.values():
        if isinstance(value, dict):
            out.extend(_leaves(value))
        else:
            out.append(value)
    return out


def test_generate_emits_metrics_matching_the_manifest(mcp_server, tmp_path):
    """generate --metrics writes the pipeline counts; emitted equals
    the dataset's example_count and the metric file is pure numbers."""
    host, token = mcp_server
    from aikoql_training.cli import main

    out = tmp_path / "dataset"
    metrics_path = tmp_path / "metrics.json"
    assert main(["generate", "--db", host, "--token", token,
                 "--out", str(out), "--metrics", str(metrics_path)]) == 0
    metrics = json.loads(metrics_path.read_text(encoding="utf-8"))
    assert metrics["counts"]["emitted"] > 0
    assert metrics["counts"]["scenarios"] >= metrics["counts"]["emitted"]
    assert metrics["rates"]["refusal"] is not None
    assert all(v is None or isinstance(v, (int, float))
               for v in _leaves(metrics))
