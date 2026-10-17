"""T-12 RED: dataset gate configuration parser (FZ-T3).

The CLI and the gate runner take one optional config file (YAML, JSON
accepted). Fail-closed everywhere: unknown keys, wrong types, negative
ratios and unreadable files raise DatasetError; NO security option
defaults permissively in silence — the absence of `secret_scan` means
the secrets gate is ON, and an explicit `false` is the only way off.
Every test below fails against the current tree:
`aikoql_training.dataset.config` does not exist.
"""

from __future__ import annotations

import json

import pytest

from aikoql_training.dataset.config import DEFAULT_CONFIG, load_config
from aikoql_training.errors import DatasetError


def test_no_file_gives_the_fail_closed_defaults():
    cfg = load_config(None)
    assert cfg["secret_scan"] is True
    assert cfg["ratios"] == [8, 1, 1]
    assert cfg["max_duplicate_rate"] == 0.0
    assert cfg is DEFAULT_CONFIG


def test_yaml_file_merges_over_defaults(tmp_path):
    p = tmp_path / "cfg.yaml"
    p.write_text("ratios: [7, 2, 1]\nmax_duplicate_rate: 0.02\nseed: 11\n",
                 encoding="utf-8")
    cfg = load_config(str(p))
    assert cfg["ratios"] == [7, 2, 1]
    assert cfg["max_duplicate_rate"] == 0.02
    assert cfg["seed"] == 11
    assert cfg["secret_scan"] is True  # untouched default survives


def test_json_file_accepted(tmp_path):
    p = tmp_path / "cfg.json"
    p.write_text(json.dumps({"ratios": [6, 2, 2]}), encoding="utf-8")
    assert load_config(str(p))["ratios"] == [6, 2, 2]


def test_explicit_secret_scan_false_is_respected(tmp_path):
    p = tmp_path / "cfg.yaml"
    p.write_text("secret_scan: false\n", encoding="utf-8")
    assert load_config(str(p))["secret_scan"] is False


@pytest.mark.parametrize("text", [
    "unknown_key: 1\n",
    "ratios: [8, 1]\n",           # must be three weights
    "ratios: [-1, 1, 1]\n",       # negative weight
    "ratios: [0, 1, 1]\n",        # zero weight
    "ratios: [\"8\", \"1\", \"1\"]\n",  # strings, not ints
    "seed: \"abc\"\n",
    "max_duplicate_rate: 2\n",    # not a rate
    "max_duplicate_rate: \"0.5\"\n",   # string, not float
    "secret_scan: \"yes\"\n",     # truthy string must NOT coerce
])
def test_malformed_configs_are_rejected(tmp_path, text):
    p = tmp_path / "cfg.yaml"
    p.write_text(text, encoding="utf-8")
    with pytest.raises(DatasetError):
        load_config(str(p))


def test_missing_config_file_refused(tmp_path):
    with pytest.raises(DatasetError):
        load_config(str(tmp_path / "nope.yaml"))


def test_malformed_yaml_refused(tmp_path):
    p = tmp_path / "cfg.yaml"
    p.write_text("ratios: [8, : ::\n", encoding="utf-8")
    with pytest.raises(DatasetError):
        load_config(str(p))
