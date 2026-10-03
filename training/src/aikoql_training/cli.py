"""The aikoql-training CLI (design §26): snapshot / generate / validate /
stats / export.

generate is the end-to-end pipeline on a live fixture DB: seed two
services + a DEPENDS_ON edge, capture the snapshot, derive factual and
relation scenarios, prove every query through the oracle, compile the
context for each question through the server's Context Compiler,
certify the answer against it (refused examples are never emitted),
split, write the canonical dataset, re-run to a scratch dir to prove
byte-identical regeneration, and validate every §5 gate — exit 0 iff
publishable.

validate exits 0 iff the report is publishable; stats and export never
judge — they count and copy.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
from typing import Any, Dict, List, Optional

from aikoql_training.client import capture_from_agent
from aikoql_training.context import compile_context
from aikoql_training.dataset.config import load_config
from aikoql_training.dataset.gates import validate_dataset
from aikoql_training.dataset.splitter import assign_splits
from aikoql_training.dataset.writer import read_dataset, write_dataset
from aikoql_training.errors import TrainingDataError
from aikoql_training.generators.answer import build_answer
from aikoql_training.generators.query import build_queries
from aikoql_training.models import GENERATOR_VERSION, SCHEMA_VERSION, compute_id
from aikoql_training.scenarios.factual import factual_scenarios
from aikoql_training.scenarios.relation import relation_scenarios
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.validation.execution import verify_scenario

_EV = {"document_id": "fixture.md", "extractor": "mock-v1", "confidence": 0.75}
_SPLITS = ("train", "val", "test")


def _emit(obj: Any) -> None:
    print(json.dumps(obj, sort_keys=True, separators=(",", ":")))


def _fixture_ir(kos: List[dict], edge: dict) -> Dict[str, Any]:
    """The mocked-ir knowledge document (the T-06 live pattern): one
    entity and one fact per scalar property, plus the relation fact —
    the statements the answers must trace to."""
    by = {k["koid"]: k for k in kos}
    entities, facts = [], []
    for ko in sorted(kos, key=lambda k: k["koid"]):
        name = str(ko["properties"].get("name"))
        entities.append({"name": name, "type_hint": ko["type_name"],
                         "mentions": [name], "confidence": 0.9,
                         "evidence": _EV})
        for prop in sorted(ko["properties"]):
            value = ko["properties"][prop]
            if isinstance(value, (str, int, float)):
                facts.append({"statement": f"The {prop} of the "
                                          f"{ko['type_name']} {name} is "
                                          f"{value}",
                              "entities": [name], "confidence": 0.9,
                              "evidence": _EV})
    f, t = by[edge["from"]], by[edge["to"]]
    facts.append({"statement": f"{f['type_name']} '{f['properties']['name']}' "
                               f"depends on {t['type_name']} "
                               f"'{t['properties']['name']}'",
                  "entities": [f["properties"]["name"],
                               t["properties"]["name"]],
                  "confidence": 0.9, "evidence": _EV})
    return {"entities": entities, "relations": [], "facts": facts,
            "events": [], "temporal": [], "page_count": 1,
            "extractor": "mock-v1"}


def _assemble(scenario: Scenario, snap, query: str, ctx: dict,
              answer: dict) -> Dict[str, Any]:
    example = {
        "schema_version": SCHEMA_VERSION,
        "generator_version": GENERATOR_VERSION,
        "source": {
            "database_id": snap.database_id,
            "snapshot_id": snap.snapshot_id,
            "knowledge_revision": snap.knowledge_revision,
            "scenario_id": scenario.scenario_id,
            "created_at": snap.created_at,
        },
        "task": {"type": scenario.task_type,
                 "difficulty": scenario.difficulty, "requires": []},
        "input": {"question": scenario.question},
        "semantic_target": {"operation": "query"},
        "query_target": {"language": "aikoql", "query": query},
        "context": {"entities": ctx["entities"], "facts": ctx["facts"],
                    "relations": ctx["relations"], "evidence": ctx["evidence"]},
        "expected": {"answer": answer["answer"], "koids": list(scenario.koids),
                     "evidence_ids": answer["evidence_ids"]},
        "policy": {"authorization_required": False},
        "labels": answer["labels"],
        # the koid component's canonical key: every example sharing any
        # koid lands in the same bucket, so cross-holdout pairs are
        # impossible by construction (the leakage gate verifies it)
        "split_key": ":".join(sorted(set(scenario.koids)))
        or scenario.scenario_id,
    }
    example["example_id"] = compute_id(example)
    return example


def _write(splits, out, snap, dataset_id):
    return write_dataset(splits, out, dataset_id=dataset_id, seed=snap.seed,
                         snapshot_id=snap.snapshot_id,
                         configuration_hash=snap.configuration_hash,
                         created_at=snap.created_at)


def cmd_snapshot(args) -> int:
    import aikoql
    with aikoql.Agent.connect(args.db, token=args.token) as db:
        snap = capture_from_agent(db, args.database_id,
                                  config=load_config(args.config),
                                  seed=args.seed)
    _emit(snap.to_dict())
    return 0


def cmd_generate(args) -> int:
    import aikoql
    config = load_config(args.config)
    with aikoql.Agent.connect(args.db, token=args.token) as db:
        # the fixture DB: two services + one edge (the house POC shape)
        a = db.remember("service", {"name": "settlement",
                                    "owner": "Payments Team", "tier": 1})
        b = db.remember("service", {"name": "checkout",
                                    "owner": "Payments Team"})
        db.relate(a["koid"], b["koid"], "DEPENDS_ON")
        kos = [db.get(a["koid"]), db.get(b["koid"])]
        edge = {"from": a["koid"], "rel": "DEPENDS_ON", "to": b["koid"]}

        snap = capture_from_agent(db, args.database_id, config=config,
                                  seed=args.seed)
        scenarios = (factual_scenarios(kos)
                     + relation_scenarios([edge], kos, "DEPENDS_ON"))
        doc = db.remember("KnowledgeSnapshot",
                          {"ir_json": json.dumps(_fixture_ir(kos, edge))})
        examples = []
        for s in scenarios:
            queries = build_queries(s, kos)
            if not queries:
                continue  # unexpressible scenario: skipped, compile stays green
            verdict = verify_scenario(db, s, queries)
            if not verdict["ok"]:
                raise TrainingDataError(
                    f"oracle failed for {s.scenario_id}: {verdict['errors']}")
            ctx = compile_context(db, doc["koid"], s.question)
            answer = build_answer(s, ctx)
            if answer is None:
                continue  # refused: an unsupported claim is never emitted
            examples.append(_assemble(s, snap, queries[0], ctx, answer))
    if not examples:
        raise TrainingDataError("generate produced no examples")

    splits, violations = assign_splits(examples, snap.seed, config["ratios"])
    if violations:
        raise TrainingDataError(
            f"split leakage: {len(violations)} cross-holdout pair(s)")
    dataset_id = config.get("dataset_id") or args.database_id
    _write(splits, args.out, snap, dataset_id)

    scratch = tempfile.mkdtemp(prefix="aikoql-tr-")
    try:
        _write(splits, scratch, snap, dataset_id)  # determinism re-run
        report = validate_dataset(args.out, db=args.db, token=args.token,
                                  config_path=args.config, reference=scratch)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    _emit(report)
    return 0 if report["publishable"] else 1


def cmd_validate(args) -> int:
    report = validate_dataset(args.dataset, db=args.db, token=args.token,
                              config_path=args.config,
                              reference=args.reference)
    _emit(report)
    return 0 if report["publishable"] else 1


def cmd_stats(args) -> int:
    got = read_dataset(args.dataset)
    _emit({"example_count": got["manifest"]["example_count"],
           "splits": {n: len(got[n]) for n in _SPLITS}})
    return 0


def cmd_export(args) -> int:
    got = read_dataset(args.src)
    m = got["manifest"]
    write_dataset({n: got[n] for n in _SPLITS}, args.out,
                  dataset_id=m["dataset_id"], seed=m["seed"],
                  snapshot_id=m["snapshot_id"],
                  configuration_hash=m["configuration_hash"],
                  created_at=m["created_at"])
    return 0


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="aikoql-training")
    sub = p.add_subparsers(dest="command", required=True)

    def live(cmd):
        cmd.add_argument("--db", required=True, help="host:port of aikoql-mcp")
        cmd.add_argument("--token", required=True, help="TCP access token")
        cmd.add_argument("--config", help="optional YAML/JSON gate config")
        cmd.add_argument("--database-id", default="acmepay",
                         help="operator database identity")
        cmd.add_argument("--seed", type=int, default=0,
                         help="generation seed")

    snap = sub.add_parser("snapshot", help="capture a dataset snapshot")
    live(snap)

    gen = sub.add_parser("generate",
                         help="seed -> snapshot -> scenarios -> dataset -> "
                              "validated report")
    live(gen)
    gen.add_argument("--out", required=True, help="dataset output directory")

    val = sub.add_parser("validate", help="run every §5 gate on a dataset")
    val.add_argument("dataset")
    val.add_argument("--db", help="host:port — enables the live gates")
    val.add_argument("--token", help="TCP access token (required with --db)")
    val.add_argument("--config", help="optional YAML/JSON gate config")
    val.add_argument("--reference",
                     help="reference dataset dir for the determinism gate")

    st = sub.add_parser("stats", help="count examples per split")
    st.add_argument("dataset")

    ex = sub.add_parser("export", help="copy a canonical dataset")
    ex.add_argument("src")
    ex.add_argument("--out", required=True, help="destination directory")

    return p


def main(argv: Optional[List[str]] = None) -> int:
    args = _parser().parse_args(argv)
    try:
        return {
            "snapshot": cmd_snapshot,
            "generate": cmd_generate,
            "validate": cmd_validate,
            "stats": cmd_stats,
            "export": cmd_export,
        }[args.command](args)
    except TrainingDataError as e:
        print(f"aikoql-training: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
