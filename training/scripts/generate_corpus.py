"""T-14: the AcmePay POC corpus (design Phase 17, §35-37).

Seeds the AcmePay KB slice per sweep seed (the §35 counts: 24
services, 6 teams, 12 persons, 8 accounts, 4 regions, 4 relation
families, 3 versioned services, 1 seeded conflict), drives every
scenario family through the real oracle + compiler, assembles examples
with component-root split keys (the leakage gate cannot find a
cross-holdout pair), and publishes only a fully gated dataset: the
§26 validator plus the E1-E9 eval set. `--reuse` regenerates from a
server the corpus already seeded (recovery through the public surface
only). A planted-secret check rides `cli validate` on the artifact.

Exit 0 iff the dataset is publishable. The report is one JSON line on
stdout: {publishable, example_count, seeds, families, refused, gates,
eval}.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path
from typing import Any, Dict, List

import aikoql

from aikoql_training.client import capture_from_agent, scan_edges
from aikoql_training.context import compile_context
from aikoql_training.dataset.config import load_config
from aikoql_training.dataset.gates import validate_dataset
from aikoql_training.dataset.splitter import assign_splits, component_ids
from aikoql_training.dataset.writer import read_dataset, write_dataset
from aikoql_training.errors import TrainingDataError
from aikoql_training.generators import build_queries
from aikoql_training.generators.answer import build_answer
from aikoql_training.models import GENERATOR_VERSION, SCHEMA_VERSION, compute_id
from aikoql_training.scenarios.ambiguity import ambiguity_scenarios
from aikoql_training.scenarios.authorization import authorization_scenarios
from aikoql_training.scenarios.contradiction import contradiction_scenarios
from aikoql_training.scenarios.factual import _eligible, factual_scenarios
from aikoql_training.scenarios.multi_hop import multi_hop_scenarios
from aikoql_training.scenarios.provenance import provenance_scenarios
from aikoql_training.scenarios.relation import relation_scenarios
from aikoql_training.scenarios.temporal import _month, temporal_scenarios
from aikoql_training.scenarios.templates import ref_of
from aikoql_training.scenarios.unknown import unknown_scenarios
from aikoql_training.validation import verify_scenario
from aikoql_training.validation.eval_set import eval_dataset

_TYPES = ("service", "team", "person", "account", "region")
_RELS = ("OWNS", "DEPENDS_ON", "WORKS_IN", "IN")
_FOCUS = ("payments", "settlement", "fraud", "ledger", "checkout", "billing")
_BUDGET = 100000
_VERSIONED = 3


def _ev(document_id: str, page: int) -> Dict[str, Any]:
    return {"document_id": document_id, "extractor": "corpus-v1",
            "page": page, "confidence": 0.75}


def _fact(statement: str, ev: Dict[str, Any]) -> Dict[str, Any]:
    # entities=[] everywhere: compiled-entity survival is task-dependent
    # (the live compiler drops entity rows that share no tokens with the
    # task), so fact-level entity citations would make E2 flaky — E2 is
    # still a real gate via the eval-set fixtures + mutation leg.
    return {"statement": statement, "entities": [], "confidence": 0.9,
            "evidence": ev}


def _entity(name: str, type_hint: str, ev: Dict[str, Any]) -> Dict[str, Any]:
    return {"name": name, "type_hint": type_hint, "mentions": [name],
            "confidence": 0.9, "evidence": ev}


def _remember_doc(db, ir: Dict[str, Any]) -> str:
    return db.remember("KnowledgeSnapshot", {"ir_json": json.dumps(ir)})["koid"]


def _history(db, koid: str) -> Dict[str, Any]:
    """The T-08 live shape: trace versions with the packed HLC decoded
    (>> 16) and each version's snapshot re-read through AS_OF."""
    versions = db._backend.trace(koid)["versions"]

    def ms(v):
        return v["commit_ts"] >> 16

    def snapshot(commit_ts):
        rows = db.aikoql(
            f"MATCH service AS_OF {commit_ts} RETURN *")["results"]
        return next(r for r in rows if r["koid"] == koid)["properties"]

    return {
        "koid": koid, "type_name": "service",
        "properties": snapshot(ms(versions[-1])),
        "versions": [{"version": v["version"], "commit_ts": ms(v),
                      "properties": snapshot(ms(v))} for v in versions],
    }


def _seed_slice(db, s: int, state: Dict[str, Any]) -> None:
    """One AcmePay slice: the §35 counts, the ownership/dependency
    graph, three versioned services and one seeded conflict. Every
    shared scalar value is seed-distinct — a cross-seed ambiguity group
    would join components and leak across holdouts."""
    kos, edges = state["kos"], state["edges"]

    def remember(type_name, props):
        row = db.get(db.remember(type_name, props)["koid"])
        kos.append(row)
        return row

    def link(f, t, rel):
        db.relate(f["koid"], t["koid"], rel)
        edges.append({"from": f["koid"], "rel": rel, "to": t["koid"]})

    teams = [remember("team", {
        "name": f"team-{s:02d}-{i:02d}",
        "lead": f"person-{s:02d}-{i * 2:02d}",
        "focus": f"{_FOCUS[i]}-{s:02d}",
    }) for i in range(6)]
    svcs = [remember("service", {
        "name": f"svc-{s:02d}-{i:02d}",
        "owner": f"team-{s:02d}-{i % 6:02d}",
        "tier": s * 3 + i % 3 + 1,
        "status": ("active" if i % 2 == 0 else "beta") + f"-{s:02d}",
    }) for i in range(24)]
    persons = [remember("person", {
        "name": f"person-{s:02d}-{i:02d}",
        "role": ("auditor" if i % 4 == 0 else "engineer") + f"-{s:02d}",
        "team": f"team-{s:02d}-{i % 6:02d}",
    }) for i in range(12)]
    accounts = [remember("account", {
        "name": f"acct-{s:02d}-{i:02d}",
        "balance": 1000 + s * 100 + i * 17,
        "currency": f"USD-{s:02d}-{i:02d}",
    }) for i in range(8)]
    regions = [remember("region", {
        "name": f"region-{s:02d}-{i:02d}",
        "sla": f"sla-{s:02d}-{i:02d}",
    }) for i in range(4)]

    # three versioned services: the owner re-remembered on the same
    # KOID after a real-time gap so the packed-HLC millis differ (AS_OF
    # must distinguish v1 from v2). MUST run before the link block
    # below: a remember()-with-koid update replaces caller-created
    # edges wholesale (remember() semantics, kernel.rs), so an edge
    # linked before the update would be silently orphaned from the
    # relationship index and TRAVERSE would go empty.
    for i in range(_VERSIONED):
        ko = svcs[i]
        time.sleep(1.05)
        db.remember("service", {
            "name": ko["properties"]["name"],
            "owner": f"legacy-owner-{s:02d}-{i:02d}",
            "tier": ko["properties"]["tier"],
            "status": ko["properties"]["status"],
        }, koid=ko["koid"])
        idx = next(j for j, r in enumerate(kos) if r["koid"] == ko["koid"])
        kos[idx] = db.get(ko["koid"])
        state["histories"].append(_history(db, ko["koid"]))

    for i, svc in enumerate(svcs):
        link(teams[i % 6], svc, "OWNS")
        if i < 23:
            link(svc, svcs[i + 1], "DEPENDS_ON")
    for i, person in enumerate(persons):
        link(person, teams[i % 6], "WORKS_IN")
    for i, account in enumerate(accounts):
        link(account, regions[i % 4], "IN")

    claim = db.get(svcs[10]["koid"])
    r = db._backend.call_tool("contradict", {
        "claim": claim["koid"], "counter_type": "service",
        "properties": {"name": claim["properties"]["name"],
                       "owner": f"Rival Owner {s:02d}"},
        "evidence": [{"source_artifact": "audit.md",
                      "method": "doc_extraction", "confidence": 0.75}],
    })
    state["counter_koids"].add(r["counter"])
    kos.append(db.get(r["counter"]))
    edges.append({"from": r["counter"], "rel": "contradicts",
                  "to": claim["koid"]})
    state["conflicts"].append({"conflict": db.get(r["conflict"]),
                               "claims": [claim, db.get(r["counter"])]})


def _ko_docs(db, slice_kos, all_kos, edges, histories, skip, docs) -> None:
    """One IR document per KO: its scalar prop facts, the incident
    relation-edge facts (both directions — a relation answer traces to
    either endpoint's doc) and, for versioned services, the v1 'was'
    facts. Contradicts edges and counters carry no document."""
    by_koid = {k["koid"]: k for k in all_kos}
    incident: Dict[str, list] = {}
    for e in edges:
        if e["rel"] in _RELS:
            incident.setdefault(e["from"], []).append(e)
            incident.setdefault(e["to"], []).append(e)
    was = {}
    for h in histories:
        v1 = next(v for v in h["versions"] if v["version"] == 1)
        was[h["koid"]] = (v1["properties"], _month(v1["commit_ts"]))

    for ko in sorted(slice_kos, key=lambda k: k["koid"]):
        if ko["koid"] in skip:
            continue
        name = ko["properties"].get("name")
        ev = _ev(f"acmepay-{ko['koid'][:8]}.md", 0)
        facts = []
        for prop in sorted(ko["properties"]):
            value = ko["properties"][prop]
            if not _eligible(value):
                continue
            facts.append(_fact(
                f"The {prop} of the {ko['type_name']} {name} is {value}",
                ev))
        for e in sorted(incident.get(ko["koid"], ()),
                        key=lambda x: (x["from"], x["rel"], x["to"])):
            f, t = by_koid[e["from"]], by_koid[e["to"]]
            rel_word = e["rel"].lower().replace("_", " ")
            facts.append(_fact(
                f"{f['type_name']} '{f['properties']['name']}' {rel_word} "
                f"{t['type_name']} '{t['properties']['name']}'", ev))
        if ko["koid"] in was:
            v1_props, label = was[ko["koid"]]
            for prop in sorted(v1_props):
                value = v1_props[prop]
                if not _eligible(value):
                    continue
                facts.append(_fact(
                    f"The {prop} of the {ko['type_name']} {name} was "
                    f"{value} in {label}", ev))
        docs["ko"][ko["koid"]] = _remember_doc(db, {
            "entities": [_entity(name, ko["type_name"], ev)],
            "relations": [], "facts": facts, "events": [], "temporal": [],
            "page_count": 1, "extractor": "corpus-v1",
        })


def _group_docs(db, scenarios, by_koid, docs) -> None:
    """One IR document per ambiguity group: every member's entities and
    prop facts — the enumeration answers trace member by member."""
    seen = set()
    for s in scenarios:
        key = (s.type_name, s.anchor_prop, str(s.anchor_value))
        if key in seen:
            continue
        seen.add(key)
        members = [by_koid[k] for k in s.koids]
        ev = _ev(f"acmepay-group-{members[0]['koid'][:8]}.md", 1)
        entities = [_entity(m["properties"]["name"], m["type_name"], ev)
                    for m in members]
        facts = []
        for m in members:
            name = m["properties"]["name"]
            for prop in sorted(m["properties"]):
                value = m["properties"][prop]
                if not _eligible(value):
                    continue
                facts.append(_fact(
                    f"The {prop} of the {m['type_name']} {name} is {value}",
                    ev))
        docs["group"][key] = _remember_doc(db, {
            "entities": entities, "relations": [], "facts": facts,
            "events": [], "temporal": [], "page_count": 1,
            "extractor": "corpus-v1",
        })


def _conflict_docs(db, conflicts, docs) -> None:
    for entry in conflicts:
        conflict = entry["conflict"]
        ev = _ev(f"acmepay-conflict-{conflict['koid'][:8]}.md", 1)
        entities, facts = [], []
        for c in entry["claims"]:
            name = c["properties"].get("name")
            entities.append(_entity(name, c["type_name"], ev))
            for prop in sorted(c["properties"]):
                value = c["properties"][prop]
                if not _eligible(value):
                    continue
                facts.append(_fact(
                    f"The {prop} of the {c['type_name']} {name} is {value}",
                    ev))
        docs["conflict"][conflict["koid"]] = _remember_doc(db, {
            "entities": entities, "relations": [], "facts": facts,
            "events": [], "temporal": [], "page_count": 1,
            "extractor": "corpus-v1",
        })


def _auth_docs(db, scenarios, docs) -> None:
    facts_by: Dict[tuple, list] = {}
    for s in scenarios:
        key = (s.subject, s.action, s.expected_answer.startswith("ALLOWED:"))
        facts_by.setdefault(key, []).append(s.expected_answer)
    for (principal, action, allowed), answers in facts_by.items():
        ev = _ev(f"acmepay-policy-{principal}-{action}-"
                 f"{'allow' if allowed else 'deny'}.md", 1)
        docs["auth"][(principal, action, allowed)] = _remember_doc(db, {
            "entities": [], "relations": [],
            "facts": [_fact(f"Policy decision: {a}", ev)
                      for a in sorted(set(answers))],
            "events": [], "temporal": [], "page_count": 1,
            "extractor": "corpus-v1",
        })


def _seam(db, kos, by_koid, docs) -> None:
    """The T-08 provenance seam: the compiled evidence rows of the KO's
    own document become the KO's citable evidence (canonical kernel
    evidence is a different shape — uncitable)."""
    for ko in kos:
        if ko["koid"] not in docs["ko"]:
            continue
        ref = ref_of(ko["koid"], by_koid)
        ctx = compile_context(db, docs["ko"][ko["koid"]],
                              f"What is the owner of {ref}?",
                              token_budget=_BUDGET)
        ko["evidence"] = ctx["evidence"]


def _decisions(db) -> List[Dict[str, Any]]:
    """The policy evaluations the authorization family proves against:
    deploy (Debug-capitalized action) then evaluate (lowercase) — the
    kernel compares against format!("{:?}", action)."""
    for name, effect, principal, action, resource_type in (
        ("auditor-read-service", "Allow", "auditor-00", "Read", "service"),
        ("auditor-write-service", "Deny", "auditor-00", "Write", "service"),
        ("auditor-read-account", "Deny", "auditor-00", "Read", "account"),
        ("engineer-read-account", "Allow", "engineer-00", "Read", "account"),
    ):
        db._backend.call_tool("deploy_policy", {
            "name": name, "effect": effect, "principal": principal,
            "action": action, "resource_type": resource_type,
        })
    decisions = []
    for principal, action, resource_type in (
        ("auditor-00", "read", "service"),
        ("auditor-00", "write", "service"),
        ("auditor-00", "read", "account"),
        ("engineer-00", "read", "account"),
    ):
        verdict = db._backend.call_tool("evaluate_policies", {
            "principal": principal, "action": action,
            "resource_type": resource_type,
        })
        decisions.append({
            "principal": principal, "action": action,
            "resource_type": resource_type,
            "allowed": bool(verdict.get("allowed")),
            "reason": verdict.get("reason") or "",
        })
    return decisions


def _pick_doc(s, docs, min_svc):
    if s.task_type == "authorization":
        return docs["auth"][(s.subject, s.action,
                             s.expected_answer.startswith("ALLOWED:"))]
    if s.task_type == "contradiction":
        return docs["conflict"][s.koids[2]]
    if s.task_type == "ambiguity":
        return docs["group"][(s.type_name, s.anchor_prop,
                              str(s.anchor_value))]
    if s.task_type == "grounded_qa" and s.difficulty in ("one_hop",
                                                         "multi_hop"):
        return docs["ko"][s.koids[1]]
    if s.task_type == "unknown" and not s.koids:
        return docs["ko"][min_svc]
    return docs["ko"][s.koids[0]]


def _assemble(s, snap, query, ctx, answer, comp, conflict_koids):
    koids = [k for k in s.koids if k not in conflict_koids]
    example = {
        "schema_version": SCHEMA_VERSION,
        "generator_version": GENERATOR_VERSION,
        "source": {
            "database_id": snap.database_id,
            "snapshot_id": snap.snapshot_id,
            "knowledge_revision": snap.knowledge_revision,
            "scenario_id": s.scenario_id,
            "created_at": snap.created_at,
        },
        "task": {"type": s.task_type, "difficulty": s.difficulty,
                 "requires": []},
        "input": {"question": s.question},
        "semantic_target": {"operation": "query"},
        "query_target": {"language": "aikoql", "query": query},
        "context": {"entities": ctx["entities"], "facts": ctx["facts"],
                    "relations": ctx["relations"], "evidence": ctx["evidence"]},
        "expected": {"answer": answer["answer"], "koids": koids,
                     "evidence_ids": answer["evidence_ids"]},
        "policy": {"authorization_required":
                   s.task_type == "authorization"},
        "labels": answer["labels"],
        "split_key": ":".join(sorted({comp.get(k, k) for k in koids}))
        or s.scenario_id,
    }
    example["example_id"] = compute_id(example)
    return example


def _generate(db, state, snap, docs, seen):
    """One pass over every scenario family: oracle-verify (fail-loud),
    compile the doc the answer must trace to, certify the answer and
    assemble. Refusals are counted and skipped — an unsupported claim
    is never emitted."""
    kos, edges = state["kos"], state["edges"]
    counter_koids = state["counter_koids"]
    conflict_koids = {c["conflict"]["koid"] for c in state["conflicts"]}
    gen_kos = [k for k in kos if k["koid"] not in counter_koids]
    gen_edges = [e for e in edges if e["rel"] in _RELS]
    by_koid = {k["koid"]: k for k in kos}
    min_svc = min(k["koid"] for k in gen_kos if k["type_name"] == "service")

    scenarios = []
    scenarios += factual_scenarios(gen_kos)
    for rel in _RELS:
        scenarios += relation_scenarios(gen_edges, gen_kos, rel)
    mh = multi_hop_scenarios(gen_edges, gen_kos)
    # The example contract stores ONE query; a mixed-rel path needs one
    # query per hop, so its stored query can never recover the far hop
    # (scenario_match). Same-rel paths ride a single DEPTH-n query
    # whose closure covers every hop.
    scenarios += [s for s in mh
                  if s.expected_path[0][1] == s.expected_path[1][1]]
    scenarios += temporal_scenarios(state["histories"])
    scenarios += unknown_scenarios(gen_kos, [
        {"type_name": "service", "property": "tier", "name": "ghost-service"},
        {"type_name": "service", "property": "color", "koid": min_svc},
    ])
    scenarios += ambiguity_scenarios(gen_kos)
    scenarios += contradiction_scenarios(state["conflicts"])
    scenarios += authorization_scenarios(gen_kos, state["decisions"])
    scenarios += provenance_scenarios(gen_kos)

    _group_docs(db, [s for s in scenarios if s.task_type == "ambiguity"],
                by_koid, docs)
    _conflict_docs(db, [c for c in state["conflicts"]
                        if c["conflict"]["koid"] not in docs["conflict"]],
                   docs)
    _auth_docs(db, [s for s in scenarios
                    if s.task_type == "authorization"], docs)

    # ALL state edges, contradicts included: the seeded counter-claim
    # must join the claim's component or the contradiction example's
    # joined key (root:counter) hashes apart from the factual example's
    # (root) while both share the claim koid — a cross-holdout leak.
    comp = component_ids(state["edges"])
    examples, refused = [], 0
    families = Counter()
    for s in scenarios:
        if s.scenario_id in seen:
            continue
        queries = build_queries(s, gen_kos)
        report = verify_scenario(db, s, queries)
        if not report["ok"]:
            raise TrainingDataError(
                f"oracle rejected {s.scenario_id}: {report['errors']}",
                stage="corpus", scenario=s.scenario_id, code="ORACLE")
        doc = _pick_doc(s, docs, min_svc)
        ctx = compile_context(db, doc, s.question, token_budget=_BUDGET)
        answer = build_answer(s, ctx)
        if answer is None:
            refused += 1
            continue
        examples.append(_assemble(s, snap, queries[0], ctx, answer, comp,
                                  conflict_koids))
        seen.add(s.scenario_id)
        if s.task_type == "grounded_qa":
            families[f"grounded_qa:{s.difficulty}"] += 1
        else:
            families[s.task_type] += 1
    return examples, refused, families


def _recover(db, state) -> None:
    """Recovery through the public surface only: type scans, the edge
    scan, the conflict scan and per-service traces (MATCH rows are the
    current versions, so versioned services surface via trace)."""
    kos, edges = state["kos"], state["edges"]
    for type_name in _TYPES:
        rows = db.aikoql(f"MATCH {type_name} RETURN *")["results"]
        kos.extend(sorted(rows, key=lambda r: r["koid"]))
    edges.extend(scan_edges(db, [k["koid"] for k in kos]))
    conf_rows = sorted(db.aikoql("MATCH aikoql:conflict RETURN *")["results"],
                       key=lambda r: r["koid"])
    for row in conf_rows:
        claims = [db.get(row["properties"]["claim_a"]),
                  db.get(row["properties"]["claim_b"])]
        state["counter_koids"].add(row["properties"]["claim_b"])
        state["conflicts"].append({"conflict": row, "claims": claims})
    for ko in list(kos):
        if ko["type_name"] != "service":
            continue
        versions = db._backend.trace(ko["koid"])["versions"]
        if len(versions) > 1:
            state["histories"].append(_history(db, ko["koid"]))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--db", required=True, help="server host:port")
    parser.add_argument("--token", required=True, help="client token")
    parser.add_argument("--database-id", default="acmepay-poc")
    parser.add_argument("--config", default=None)
    parser.add_argument("--out", required=True, help="dataset directory")
    parser.add_argument("--target", type=int, default=10000)
    parser.add_argument("--seeds", type=int, default=16)
    parser.add_argument("--artifacts", required=True,
                        help="manifest/stats directory")
    parser.add_argument("--reuse", action="store_true",
                        help="regenerate from a server the corpus seeded")
    args = parser.parse_args(argv)

    cfg = load_config(args.config)
    state: Dict[str, Any] = {"kos": [], "edges": [], "conflicts": [],
                             "histories": [], "counter_koids": set(),
                             "decisions": []}
    docs = {"ko": {}, "group": {}, "conflict": {}, "auth": {}}
    seen: set = set()
    with aikoql.Agent.connect(args.db, token=args.token) as db:
        state["decisions"] = _decisions(db)
        if args.reuse:
            _recover(db, state)
            seeds_used = list(range(args.seeds))
        else:
            for s in range(args.seeds):
                _seed_slice(db, s, state)
            seeds_used = []
        snap = capture_from_agent(db, args.database_id, config=cfg, seed=0)

        by_koid = {k["koid"]: k for k in state["kos"]}
        examples: List[dict] = []
        refused = 0
        families: Counter = Counter()
        per_seed: Dict[int, int] = {}
        if args.reuse:
            _ko_docs(db, state["kos"], state["kos"], state["edges"],
                     state["histories"], state["counter_koids"], docs)
            _seam(db, state["kos"], by_koid, docs)
            new, refused, families = _generate(db, state, snap, docs, seen)
            examples += new
            per_seed[0] = len(examples)
        else:
            for s in range(args.seeds):
                fresh = [k for k in state["kos"] if k["koid"] not in docs["ko"]]
                _ko_docs(db, fresh, state["kos"], state["edges"],
                         state["histories"], state["counter_koids"], docs)
                _seam(db, fresh, by_koid, docs)
                new, refused, families = _generate(db, state, snap, docs,
                                                   seen)
                examples += new
                per_seed[s] = len(new)
                seeds_used.append(s)
                if len(examples) >= args.target:
                    break

        splits, violations = assign_splits(examples, snap.seed, cfg["ratios"])
        if violations:
            raise TrainingDataError(f"{len(violations)} cross-holdout pairs",
                                    stage="splits", code="LEAKAGE")
        write_dataset(splits, args.out, dataset_id=args.database_id,
                      seed=snap.seed, snapshot_id=snap.snapshot_id,
                      configuration_hash=snap.configuration_hash,
                      created_at=snap.created_at)
        scratch = tempfile.mkdtemp(prefix="corpus-determinism-")
        try:
            write_dataset(splits, scratch, dataset_id=args.database_id,
                          seed=snap.seed, snapshot_id=snap.snapshot_id,
                          configuration_hash=snap.configuration_hash,
                          created_at=snap.created_at)
            validation = validate_dataset(args.out, db=args.db,
                                          token=args.token,
                                          config_path=args.config,
                                          reference=scratch)
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
        eval_out = eval_dataset(read_dataset(args.out))
        publishable = validation["publishable"] and all(
            c["ok"] for c in eval_out.values())
        report = {"publishable": publishable,
                  "example_count": validation["example_count"],
                  "seeds": seeds_used, "families": dict(families),
                  "refused": refused, "gates": validation["gates"],
                  "eval": eval_out}
        artifacts = Path(args.artifacts)
        artifacts.mkdir(parents=True, exist_ok=True)
        (artifacts / "manifest.json").write_text(
            json.dumps({"example_count": validation["example_count"],
                        "seeds": seeds_used, "target": args.target,
                        "generator_version": GENERATOR_VERSION,
                        "dataset_id": args.database_id,
                        "created_at": snap.created_at},
                       sort_keys=True, indent=2), encoding="utf-8")
        (artifacts / "stats.json").write_text(
            json.dumps({"seeds": len(per_seed), "per_seed": per_seed},
                       sort_keys=True, indent=2), encoding="utf-8")
        print(json.dumps(report, sort_keys=True))
        return 0 if publishable else 1


if __name__ == "__main__":
    sys.exit(main())
