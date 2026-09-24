#!/usr/bin/env python3
"""P5-M47 (R4-P2-04/05) — shared artifact validation for the perf gates.

gate5-check.py and perf-smoke-check.py consume the harness artifacts
field-by-field; a schema drift anywhere raised a raw KeyError with no
breadcrumb. Every field a checker consumes is now validated up front with
a NAMED error (the failing path, field, and row), and the fresh artifact's
environment.git_sha must equal the tested HEAD — stale evidence can never
silently feed a gate. The committed baseline artifacts are exempt from the
freshness check BY DESIGN: a baseline is historical, staleness is its
nature; only the fresh side of a comparison must be stamped at the head
under test.

Also the CI republish gate: `python scripts/artifact_schema.py <path>`
validates and freshness-stamps one artifact (baseline-guard's republish
job runs it on the files it uploads).
"""
import json
import subprocess
import sys


class SchemaError(Exception):
    """One named, actionable schema problem — never a raw KeyError."""

    def __init__(self, msg, unreadable=False):
        super().__init__(msg)
        # unreadable marks a missing/unopenable file: the perf-smoke checker
        # appends its "did the run set V2ADOPT_PERF_SMOKE=1?" hint only to
        # this class (a schema drift needs the schema hint, not the env one).
        self.unreadable = unreadable


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except OSError as e:
        raise SchemaError(f"{path}: unreadable: {e}", unreadable=True) from e
    except json.JSONDecodeError as e:
        raise SchemaError(f"{path}: not valid JSON: {e}") from e


def head_sha():
    """The tested HEAD — the stamp a fresh artifact must carry."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError) as e:
        raise SchemaError(f"cannot resolve tested HEAD (git rev-parse): {e}")
    return out.stdout.strip()


def _num(value, path, field):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise SchemaError(
            f"{path}: field {field} is not a number (got {type(value).__name__})"
        )
    return float(value)


def check_fresh(path, data):
    """The freshness gate: a fresh artifact must be stamped at tested HEAD."""
    env = data.get("environment")
    if (
        not isinstance(env, dict)
        or not isinstance(env.get("git_sha"), str)
        or not env["git_sha"]
    ):
        raise SchemaError(f"{path}: missing field: environment.git_sha")
    head = head_sha()
    if env["git_sha"] != head:
        raise SchemaError(
            f"{path}: stale artifact - environment.git_sha {env['git_sha']} "
            f"!= tested HEAD {head} (regenerate the artifact at this checkout)"
        )


def validate_1m(path, fresh=False):
    """Validated rows from a harness 1M artifact: {(backend, label): p50_ns}.

    fresh=True additionally enforces the git_sha stamp (the fresh side of a
    gate comparison only — baselines are historical by design).
    """
    data = load(path)
    backends = data.get("backends")
    if not isinstance(backends, list) or not backends:
        raise SchemaError(f"{path}: missing field: backends (non-empty list)")
    rows = {}
    for bi, b in enumerate(backends):
        for ri, r in enumerate(b.get("rows", []) or []):
            where = f"{path}: backends[{bi}].rows[{ri}]"
            label = r.get("label")
            if not isinstance(label, str):
                raise SchemaError(f"{where}: missing field: label")
            if "p50_ns" not in r:
                raise SchemaError(f"{where}: missing field: p50_ns (label {label!r})")
            rows[(b.get("name"), label)] = _num(r["p50_ns"], where, "p50_ns")
    if fresh:
        check_fresh(path, data)
    return rows


def validate_smoke_cells(path):
    """Validated cells from the perf-smoke baseline: {key: value}."""
    data = load(path)
    cells = data.get("cells")
    if not isinstance(cells, dict):
        raise SchemaError(f"{path}: missing field: cells")
    out = {}
    for key in ("w1_ko_get_p50_ns", "w2_head_get_p50_ns", "hot_head_p50_ns"):
        if key not in cells:
            raise SchemaError(f"{path}: missing field: {key} in baseline cells")
        out[key] = _num(cells[key], path, key)
    return out


def main():
    if len(sys.argv) != 2:
        print("usage: python scripts/artifact_schema.py <artifact.json>", file=sys.stderr)
        sys.exit(2)
    path = sys.argv[1]
    try:
        rows = validate_1m(path, fresh=True)
    except SchemaError as e:
        print(f"FRESH FAIL: {e}", file=sys.stderr)
        sys.exit(1)
    print(f"FRESH OK: {path} — {len(rows)} validated rows")


if __name__ == "__main__":
    main()
