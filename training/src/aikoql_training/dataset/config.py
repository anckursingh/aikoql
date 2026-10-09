"""The dataset gate configuration parser (FZ-T3).

One optional config file (YAML or JSON) merges over fail-closed
defaults. Unknown keys, wrong types, negative ratios and unreadable
files raise DatasetError; no security option silently defaults to
permissive — absent `secret_scan` means the gate is ON, and an
explicit `false` is the only way off.
"""

from __future__ import annotations

import json
from typing import Any, Dict, Optional

from aikoql_training.errors import DatasetError

# fail-closed defaults: the secrets gate is ON unless explicitly disabled
DEFAULT_CONFIG: Dict[str, Any] = {
    "ratios": [8, 1, 1],
    "max_duplicate_rate": 0.0,
    "secret_scan": True,
}

_KNOWN = {"ratios", "max_duplicate_rate", "secret_scan", "seed",
          "dataset_id", "database_id"}


def load_config(path: Optional[str]) -> Dict[str, Any]:
    """Load the gate/split configuration; `path` None -> the defaults."""
    if path is None:
        return DEFAULT_CONFIG
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        raise DatasetError(f"config file unreadable: {path}: {e}") from e
    try:
        if path.endswith((".yaml", ".yml")):
            import yaml
            raw = yaml.safe_load(text)
        else:
            raw = json.loads(text)
    except Exception as e:
        raise DatasetError(f"config file malformed: {path}: {e}") from e
    if raw is None:
        raw = {}
    if not isinstance(raw, dict):
        raise DatasetError(f"config must be a mapping, got {type(raw).__name__}")

    unknown = set(raw) - _KNOWN
    if unknown:
        raise DatasetError(
            f"unknown config key(s): {', '.join(sorted(unknown))}")

    cfg = dict(DEFAULT_CONFIG)
    cfg.update(raw)
    _validate(cfg)
    return cfg


def _validate(cfg: Dict[str, Any]) -> None:
    ratios = cfg["ratios"]
    if (not isinstance(ratios, (list, tuple)) or len(ratios) != 3
            or any(not isinstance(w, int) or isinstance(w, bool) or w <= 0
                   for w in ratios)):
        raise DatasetError(f"ratios must be three positive ints, got {ratios!r}")
    rate = cfg["max_duplicate_rate"]
    if (isinstance(rate, bool) or not isinstance(rate, (int, float))
            or not 0.0 <= rate <= 1.0):
        raise DatasetError(
            f"max_duplicate_rate must be a rate in [0, 1], got {rate!r}")
    if not isinstance(cfg["secret_scan"], bool):
        raise DatasetError(
            f"secret_scan must be a bool (no coercion), got "
            f"{cfg['secret_scan']!r}")
    if "seed" in cfg and (isinstance(cfg["seed"], bool)
                          or not isinstance(cfg["seed"], int)):
        raise DatasetError(f"seed must be an int, got {cfg['seed']!r}")
