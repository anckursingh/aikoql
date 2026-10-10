"""The dataset writer/reader (design Phase 14) — the publication boundary.

write_dataset: every example line is the canonical single-line
`to_json` (models.py), sorted by example_id within each split; split
files are written to temp names and atomically renamed (os.replace);
stale temp files from an interrupted run are swept at start; and
manifest.json is written LAST — its presence is dataset visibility.
The manifest carries per-split count/file/sha256 plus example_count
and the identity fields. `created_at` is an EXPLICIT operator
parameter: wall-clock in the manifest would break byte-identical
regeneration (determinism law 3).

read_dataset verifies the manifest, per-file sha256 and counts, and
refuses any tampered/truncated dataset with DatasetError (fail-closed,
FZ-T2). It returns {"manifest": ..., "train": [...], "val": [...],
"test": [...]}.
"""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Sequence

from aikoql_training.errors import DatasetError
from aikoql_training.models import GENERATOR_VERSION, SCHEMA_VERSION, to_json

_MANIFEST = "manifest.json"
_SPLITS = ("train", "val", "test")


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _atomic_write_text(path: Path, content: str) -> None:
    """Temp file + os.replace: a reader never sees a half-written file."""
    fd, tmp = tempfile.mkstemp(prefix=path.name + ".tmp", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
            f.write(content)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def _sweep_stale(dataset_dir: Path) -> None:
    """Interrupted-run cleanup: remove any temp files left behind."""
    for p in dataset_dir.iterdir():
        if ".tmp" in p.name:
            try:
                p.unlink()
            except OSError:
                pass


def write_dataset(
    examples_by_split: Dict[str, Sequence[dict]],
    dataset_dir: str,
    *,
    dataset_id: str,
    seed: int,
    snapshot_id: str,
    configuration_hash: str,
    created_at: str,
    held_out_orgs: Sequence[str] = (),
) -> Dict[str, Any]:
    """Write a canonical dataset; returns the manifest dict.

    `held_out_orgs` (T-28) names the synthetic orgs the model must
    never train on — recorded in the manifest so the leakage gate can
    recompute the assignment with the same holdout declaration."""
    d = Path(dataset_dir)
    d.mkdir(parents=True, exist_ok=True)
    _sweep_stale(d)

    splits: Dict[str, Dict[str, Any]] = {}
    total = 0
    for name in _SPLITS:
        ordered = sorted(examples_by_split[name], key=lambda e: e["example_id"])
        total += len(ordered)
        lines = "".join(to_json(e) + "\n" for e in ordered)
        file = d / f"{name}.jsonl"
        _atomic_write_text(file, lines)
        splits[name] = {"count": len(ordered), "file": file.name,
                        "sha256": _sha256(file)}

    manifest = {
        "dataset_id": dataset_id,
        "schema_version": SCHEMA_VERSION,
        "generator_version": GENERATOR_VERSION,
        "seed": seed,
        "snapshot_id": snapshot_id,
        "configuration_hash": configuration_hash,
        "created_at": created_at,
        "example_count": total,
        "splits": splits,
        "held_out_orgs": sorted(set(held_out_orgs)),
    }
    # written LAST — publication is the manifest's visibility
    _atomic_write_text(d / _MANIFEST, json.dumps(manifest, sort_keys=True,
                                                 separators=(",", ":")) + "\n")
    return manifest


def tampered_splits(dataset_dir: str, manifest: Dict[str, Any]) -> List[str]:
    """Split names whose file sha256 disagrees with the manifest ([] = intact).

    The one tamper comparison, shared by read_dataset (raises) and the
    validator (reports it as the integrity gate). The manifest cells are
    assumed structurally valid — read_dataset checks that first.
    """
    d = Path(dataset_dir)
    return [name for name in _SPLITS
            if _sha256(d / manifest["splits"][name]["file"])
            != manifest["splits"][name].get("sha256")]


def read_dataset(dataset_dir: str, *, verify: bool = True) -> Dict[str, Any]:
    """Read a canonical dataset back; verify=True checks every byte
    (FZ-T2) and refuses a tampered/truncated dataset with DatasetError.
    verify=False skips the sha256 check so a tampered dataset can still
    be gated (the validator reports integrity as a gate, then runs the
    rest over whatever is readable)."""
    d = Path(dataset_dir)
    if not d.is_dir():
        raise DatasetError(f"dataset dir not found: {d}")
    mp = d / _MANIFEST
    if not mp.is_file():
        raise DatasetError(f"dataset manifest missing: {mp}")
    try:
        manifest = json.loads(mp.read_text(encoding="utf-8"))
    except ValueError as e:
        raise DatasetError(f"dataset manifest unreadable: {e}") from e
    if not isinstance(manifest, dict) or "splits" not in manifest:
        raise DatasetError("dataset manifest malformed: no splits")

    tampered = tampered_splits(dataset_dir, manifest) if verify else []
    out: Dict[str, Any] = {"manifest": manifest}
    for name in _SPLITS:
        cell = manifest["splits"].get(name)
        if not isinstance(cell, dict) or "file" not in cell:
            raise DatasetError(f"manifest split {name} malformed")
        file = d / cell["file"]
        if not file.is_file():
            raise DatasetError(f"split file missing: {file}")
        if name in tampered:
            raise DatasetError(f"split {name} tampered: sha256 mismatch")
        try:
            # split on "\n" ONLY: splitlines() also splits on U+0085 and
            # U+2028/U+2029, which canonical ensure_ascii=False JSON emits
            # raw inside strings (FZ-T6)
            examples: List[dict] = [json.loads(line) for line in
                                    file.read_text(encoding="utf-8").split("\n")
                                    if line.strip()]
        except ValueError as e:
            raise DatasetError(f"split {name} truncated: {e}") from e
        if len(examples) != cell.get("count"):
            raise DatasetError(
                f"split {name} count mismatch: manifest {cell.get('count')}, "
                f"file {len(examples)}")
        out[name] = examples
    return out
