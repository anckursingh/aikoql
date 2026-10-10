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
validates and freshness-stamps one artifact (benchmark.yml's guard job
runs it on the files it uploads). CI-08: the competitor artifact (§13/§18)
dispatches to validate_competitor — the schema is one contract for every
artifact family the gates consume.
"""
import json
import subprocess
import sys


class SchemaError(Exception):
    """One named, actionable schema problem — never a raw KeyError."""

    def __init__(self, msg, unreadable=False):
        super().__init__(msg)
        # unreadable marks a missing/unopenable file: the perf-smoke checker
        # appends its "did the run set STORAGE_PERF_SMOKE=1?" hint only to
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
    for key in (
        "w1_ko_get_p50_ns",
        "w2_head_get_p50_ns",
        "write_p50_ns",
        "scan_p50_ns",
        "hot_head_p50_ns",
        "compact_wall_ms",
        "compact_allocs",
    ):
        if key not in cells:
            raise SchemaError(f"{path}: missing field: {key} in baseline cells")
        out[key] = _num(cells[key], path, key)
    return out


# CI-08 (§13): the competitor artifact's contract — the §13 keys every
# engine column must carry, plus the §18 digest evidence where measurable.
# The arch gate (workflow test 9) enforces the pinned tags in benchmark.yml;
# the schema enforces that whatever the harness measured is complete and
# fresh. Container cpu/mem/disk probes may be None (runners without docker
# access) — aikoql's never are (measured in-process).

COMPETITOR_ENGINES = ("aikoql", "postgresql", "neo4j", "qdrant", "mongodb")
COMPETITOR_WORKLOADS = ("point_read", "point_write", "structured_filter",
                        "transactions", "graph", "vector_recall",
                        "knowledge_query")
ENGINE_METRICS = ("cpu_seconds", "memory_mb", "disk_bytes", "ingest_s")


def validate_competitor(path, fresh=False):
    """Validated matrix rows from the competitor artifact:
    {(engine, workload): {"p50_ms": float, ...}} — the §13 schema.

    fresh=True additionally enforces the git_sha stamp (the nightly CI
    artifact is fresh; historical archive copies are not checked).
    """
    data = load(path)
    engines = data.get("engines")
    if not isinstance(engines, dict) or not engines:
        raise SchemaError(f"{path}: missing field: engines (non-empty dict)")
    rows = {}
    for key in COMPETITOR_ENGINES:
        where = f"{path}: engines[{key}]"
        e = engines.get(key)
        if not isinstance(e, dict):
            raise SchemaError(f"{where}: missing engine column")
        for f in ENGINE_METRICS:
            if f not in e:
                raise SchemaError(f"{where}: missing field: {f} (§13)")
        if e["cpu_seconds"] is not None:
            _num(e["cpu_seconds"], where, "cpu_seconds")
        if e["memory_mb"] is not None:
            _num(e["memory_mb"], where, "memory_mb")
        if e["disk_bytes"] is not None:
            _num(e["disk_bytes"], where, "disk_bytes")
        if key == "aikoql" and e["cpu_seconds"] is None:
            raise SchemaError(
                f"{where}: cpu_seconds must be measured in-process (§13)")
        wl = {w.get("name"): w for w in e.get("workloads", []) or []}
        if not wl:
            raise SchemaError(f"{where}: missing field: workloads")
        for name in COMPETITOR_WORKLOADS:
            w = wl.get(name)
            wwhere = f"{where}.workloads[{name}]"
            if not isinstance(w, dict):
                raise SchemaError(f"{wwhere}: missing workload cell")
            if w.get("n"):
                for f in ("p50_ms", "p95_ms", "p99_ms", "throughput_ops_s"):
                    if f not in w:
                        raise SchemaError(f"{wwhere}: missing field: {f}")
                rows[(key, name)] = _num(w["p50_ms"], wwhere, "p50_ms")
    for f in ("commit", "seed", "config", "dataset"):
        if f not in data:
            raise SchemaError(f"{path}: missing field: {f} (§13)")
    env = data.get("environment")
    if not isinstance(env, dict):
        raise SchemaError(f"{path}: missing field: environment (§13)")
    for f in ("os", "cpu", "ram_mb", "cache_state", "harness_sha"):
        if not env.get(f):
            raise SchemaError(f"{path}: missing field: environment.{f} (§13)")
    _num(env["ram_mb"], path, "environment.ram_mb")
    # §18: the engine_versions columns — aikoql's SDK version and, where the
    # measuring host could probe docker, the pinned tag + digest evidence.
    vers = data.get("engine_versions")
    if not isinstance(vers, dict):
        raise SchemaError(f"{path}: missing field: engine_versions (§18)")
    if not isinstance(vers.get("aikoql"), str) or not vers["aikoql"]:
        raise SchemaError(f"{path}: engine_versions.aikoql not a version string")
    for key in ("pg", "neo4j", "qdrant", "mongo"):
        v = vers.get(key)
        if v is not None:
            if not isinstance(v, dict) or not v.get("image") or not v.get("digest"):
                raise SchemaError(
                    f"{path}: engine_versions.{key} must be null or carry "
                    "image + digest (§18)")
    if fresh:
        check_fresh(path, data)
    return rows


# D-20 (§30): the tier×transport artifact's contract — the three measured
# tiers (protocol/serialization/SDK) each with the §30 metric set, the
# cited tiers (engine/application) named by their covering artifact, REST
# explicitly N/A, and the honest-labeling pin: an SDK cell whose claim
# does not say "SDK latency" — or dares to say "engine latency" — is a
# SchemaError (§30: SDK latency is never presented as engine latency).

MEASURED_TIERS = ("protocol", "serialization", "sdk")
MEASURED_TRANSPORTS = ("mcp", "native", "embedded", "transportless")
TIER_OPS = ("remember", "get", "scan", "encode", "decode")
CELL_METRICS = ("p50_ns", "p95_ns", "p99_ns", "throughput_ops_s",
                "allocs_per_op", "wire_bytes_req", "wire_bytes_resp",
                "decodes_per_op", "cpu_seconds", "rss_mb")


def validate_tiers(path, fresh=False):
    """Validated cells from the D-20 tier artifact:
    {(tier, transport, op): p50_ns} — the §30 schema.

    fresh=True additionally enforces the git_sha stamp (the committed
    certification artifact is fresh; the checker runs the same dispatch).
    """
    data = load(path)
    tiers = data.get("tiers")
    if not isinstance(tiers, dict):
        raise SchemaError(f"{path}: missing field: tiers (§30)")
    for key, label in (("engine", "engine tier"),
                       ("application", "application tier")):
        t = tiers.get(key)
        if not isinstance(t, dict) or not t.get("covered_by"):
            raise SchemaError(
                f"{path}: tiers.{key} must cite its covering artifact "
                f"(§30: the {label} is covered elsewhere, not re-measured)")
    if tiers.get("rest", {}).get("status") != "N/A — no REST transport exists":
        raise SchemaError(f"{path}: tiers.rest must be the explicit N/A")
    cells = data.get("cells")
    if not isinstance(cells, list) or not cells:
        raise SchemaError(f"{path}: missing field: cells (non-empty list)")
    rows = {}
    seen = set()
    for ci, c in enumerate(cells):
        where = f"{path}: cells[{ci}]"
        for f in ("tier", "transport", "op"):
            if not isinstance(c.get(f), str):
                raise SchemaError(f"{where}: missing field: {f}")
        if c["tier"] not in MEASURED_TIERS:
            raise SchemaError(f"{where}: unknown tier {c['tier']!r}")
        if c["transport"] not in MEASURED_TRANSPORTS:
            raise SchemaError(f"{where}: unknown transport "
                              f"{c['transport']!r}")
        if c["op"] not in TIER_OPS:
            raise SchemaError(f"{where}: unknown op {c['op']!r}")
        for f in CELL_METRICS + ("n",):
            if f not in c:
                raise SchemaError(f"{where}: missing field: {f} (§30)")
            _num(c[f], where, f)
        if c["correct"] is not True:
            raise SchemaError(
                f"{where}: correct={c['correct']!r} — an artifact with a "
                "failed oracle is uncommittable (§30)")
        if c.get("proc_scope") not in ("server", "harness"):
            raise SchemaError(f"{where}: proc_scope must name the measured "
                              "process (§30: CPU/RSS without a scope is "
                              "an unlabeled number)")
        claim = c.get("claim")
        if not isinstance(claim, str) or not claim:
            raise SchemaError(f"{where}: missing field: claim (§30: every "
                              "number carries its label)")
        if c["tier"] == "sdk" and ("SDK latency" not in claim
                                   or "engine latency" in claim):
            raise SchemaError(
                f"{where}: an SDK cell must claim 'SDK latency' and must "
                "never claim engine latency (§30)")
        key = (c["tier"], c["transport"], c["op"])
        if key in seen:
            raise SchemaError(f"{where}: duplicate cell {key}")
        seen.add(key)
        rows[key] = _num(c["p50_ns"], where, "p50_ns")
    # The certification's shape: every measured tier × transport the
    # deliverable names must carry cells (REST is N/A by design).
    for key in (("protocol", "mcp"), ("protocol", "native"),
                ("serialization", "transportless"),
                ("sdk", "mcp"), ("sdk", "embedded")):
        if not any(k[:2] == key for k in seen):
            raise SchemaError(f"{path}: no cells for {key[0]}×{key[1]} "
                              "(§30 tier×transport matrix)")
    for f in ("commit", "seed", "engine_version"):
        if f not in data:
            raise SchemaError(f"{path}: missing field: {f} (§13)")
    env = data.get("environment")
    if not isinstance(env, dict):
        raise SchemaError(f"{path}: missing field: environment (§13)")
    for f in ("os", "cpu", "ram_mb", "cache_state", "git_sha",
              "harness_sha"):
        if not env.get(f):
            raise SchemaError(f"{path}: missing field: environment.{f} (§13)")
    _num(env["ram_mb"], path, "environment.ram_mb")
    if fresh:
        check_fresh(path, data)
    return rows


def main():
    if len(sys.argv) != 2:
        print("usage: python scripts/artifact_schema.py <artifact.json>", file=sys.stderr)
        sys.exit(2)
    path = sys.argv[1]
    try:
        # CI-08: dispatch on the artifact's own shape — the competitor
        # matrix (§13) vs the 1M harness rows. D-20 adds the tiers shape.
        data = load(path)
        if "engines" in data:
            rows = validate_competitor(path, fresh=True)
        elif "backends" in data:
            rows = validate_1m(path, fresh=True)
        elif "tiers" in data:
            rows = validate_tiers(path, fresh=True)
        else:
            raise SchemaError(f"{path}: neither engines, backends nor "
                              "tiers — unknown artifact shape")
    except SchemaError as e:
        print(f"FRESH FAIL: {e}", file=sys.stderr)
        sys.exit(1)
    print(f"FRESH OK: {path} — {len(rows)} validated rows")


if __name__ == "__main__":
    main()
