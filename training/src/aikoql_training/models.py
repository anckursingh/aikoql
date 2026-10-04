"""Canonical training-example schema (design §9 + T-01 recon corrections).

Fail-closed: unknown fields, wrong types, missing required fields, forged
example IDs and non-JSON-serializable content all raise SchemaError.
Serialization is sort-keyed and separator-tight, so content-derived IDs
(design §24) and dataset checksums are stable.

Recon correction: query_target is TEXT aikoql — the public query surface
is tool_aikoql → aikoql_compiler::parser::parse (see
docs/training-data-architecture.md), not the design doc's "aikoql-json".
"""
from __future__ import annotations

import hashlib
import json
from typing import Any, Dict

from aikoql_training.errors import SchemaError

SCHEMA_VERSION = "2"
GENERATOR_VERSION = "0.2.0"

TASK_TYPES = {
    "intent", "query", "grounded_qa", "reasoning",
    "unknown", "temporal", "provenance", "authorization",
    "ambiguity", "contradiction",
}
DIFFICULTIES = {"factual", "one_hop", "multi_hop", "comparison"}
PLAN_OPS = {"resolve_entity", "traverse", "project"}
ENTITY_ROLES = {"subject", "target", "intermediate", "candidate"}

# Field spec per section: field name -> required type.
_SPEC = {
    "example_id": str,
    "schema_version": str,
    "generator_version": str,
    "source": dict,
    "task": dict,
    "input": dict,
    "semantic_target": dict,
    "query_target": dict,
    "context": dict,
    "expected": dict,
    "policy": dict,
    "labels": dict,
    "split_key": str,
}

_NESTED = {
    "source": {
        "database_id": str, "snapshot_id": str, "knowledge_revision": str,
        "scenario_id": str, "created_at": str,
    },
    "task": {"type": str, "difficulty": str, "requires": list},
    "input": {"question": str},
    "semantic_target": {"operation": str, "intent": str, "entities": list,
                        "requirements": list, "plan": dict},
    "query_target": {"language": str, "query": str},
    "context": {"entities": list, "facts": list, "relations": list, "evidence": list},
    "expected": {"answer": str, "koids": list, "evidence_ids": list},
    "policy": {"authorization_required": bool},
    "labels": {"grounded": bool, "answerable": bool, "ambiguous": bool, "contradictory": bool},
}

# Fields allowed (typed when present) but not required: the ACL pair on
# the policy section, present exactly when authorization_required is
# true (design §41); the T-22 claim decomposition on grounded answers
# ({"statement", "evidence_ids"} per claim, walked by the grounding
# validator).
_OPTIONAL = {
    "policy": {"subject": str, "action": str},
    "expected": {"claims": list},
}


def _dump(obj: Any) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def compute_id(example: Dict[str, Any]) -> str:
    """Content-derived example ID (design §24): sha256 over the canonical
    JSON of (schema_version, snapshot, task_type, question, query,
    scenario_id). Never a random UUID — regeneration is reproducible."""
    try:
        identity = {
            "schema_version": example["schema_version"],
            "snapshot_id": example["source"]["snapshot_id"],
            "task_type": example["task"]["type"],
            "question": example["input"]["question"],
            "query": example["query_target"]["query"],
            "scenario_id": example["source"]["scenario_id"],
        }
    except KeyError as exc:
        raise SchemaError(f"cannot derive example_id: missing {exc.args[0]}")
    digest = hashlib.sha256(_dump(identity).encode("utf-8")).hexdigest()
    return f"sha256:{digest}"


def to_json(example: Dict[str, Any]) -> str:
    """Canonical serialization: sort-keyed, separator-tight, single line."""
    return _dump(example)


def validate(example: Dict[str, Any]) -> None:
    """Fail-closed schema validation. Raises SchemaError; returns None."""
    if not isinstance(example, dict):
        raise SchemaError("example must be an object")
    for field, typ in _SPEC.items():
        if field not in example:
            raise SchemaError(f"missing required field: {field}")
        if not isinstance(example[field], typ):
            raise SchemaError(f"{field}: expected {typ.__name__}")
    for field in example:
        if field not in _SPEC:
            raise SchemaError(f"unknown field: {field}")
    for section, fields in _NESTED.items():
        sub = example[section]
        for field, typ in fields.items():
            if field not in sub:
                raise SchemaError(f"{section}: missing required field: {field}")
            if not isinstance(sub[field], typ):
                raise SchemaError(f"{section}.{field}: expected {typ.__name__}")
        for field in sub:
            if field not in fields and field not in _OPTIONAL.get(section, {}):
                raise SchemaError(f"unknown field: {section}.{field}")
    for section, fields in _OPTIONAL.items():
        sub = example[section]
        for field, typ in fields.items():
            if field in sub and not isinstance(sub[field], typ):
                raise SchemaError(f"{section}.{field}: expected {typ.__name__}")

    if example["schema_version"] != SCHEMA_VERSION:
        raise SchemaError(
            f"schema_version: expected {SCHEMA_VERSION}, got {example['schema_version']}")
    if example["task"]["type"] not in TASK_TYPES:
        raise SchemaError(f"task.type: unknown task type: {example['task']['type']}")
    if example["task"]["difficulty"] not in DIFFICULTIES:
        raise SchemaError(
            f"task.difficulty: unknown difficulty: {example['task']['difficulty']}")
    if not example["input"]["question"].strip():
        raise SchemaError("input.question: must not be empty")
    # Recon correction: the compiler's public surface parses TEXT aikoql.
    if example["query_target"]["language"] != "aikoql":
        raise SchemaError(
            f"query_target.language: expected 'aikoql', got {example['query_target']['language']!r}")
    if not example["query_target"]["query"].strip():
        raise SchemaError("query_target.query: must not be empty")

    # T-17 semantic target (design §41): the plan is a closed, typed
    # contract — unknown ops/roles and untyped steps are rejected.
    st = example["semantic_target"]
    if st["intent"] not in TASK_TYPES:
        raise SchemaError(
            f"semantic_target.intent: unknown task type: {st['intent']!r}")
    for entry in st["entities"]:
        if not isinstance(entry, dict):
            raise SchemaError("semantic_target.entities: entries must be objects")
        for key in entry:
            if key not in ("koid", "role"):
                raise SchemaError(f"unknown field: semantic_target.entities.{key}")
        if not isinstance(entry.get("koid"), str):
            raise SchemaError("semantic_target.entities: entry koid must be a str")
        if entry.get("role") not in ENTITY_ROLES:
            raise SchemaError(
                f"semantic_target.entities: unknown role: {entry.get('role')!r}")
    for req in st["requirements"]:
        if not isinstance(req, str) or not req.strip():
            raise SchemaError(
                "semantic_target.requirements: entries must be non-empty strings")
    plan = st["plan"]
    if not isinstance(plan.get("steps"), list):
        raise SchemaError("semantic_target.plan: missing required field: steps")
    for key in plan:
        if key not in ("steps", "temporal"):
            raise SchemaError(f"unknown field: semantic_target.plan.{key}")
    if "temporal" in plan and (not isinstance(plan["temporal"], dict)
                               or not isinstance(plan["temporal"].get("as_of"), int)):
        raise SchemaError("semantic_target.plan.temporal: needs as_of int")
    for step in plan["steps"]:
        if not isinstance(step, dict):
            raise SchemaError("semantic_target.plan.steps: entries must be objects")
        if step.get("op") not in PLAN_OPS:
            raise SchemaError(
                f"semantic_target.plan.steps: unknown op: {step.get('op')!r}")
        if step["op"] == "traverse":
            for key in ("from", "relation", "to"):
                if not isinstance(step.get(key), str) or not step[key].strip():
                    raise SchemaError(
                        f"semantic_target.plan.steps: traverse needs {key}")
        elif step["op"] == "resolve_entity":
            if not isinstance(step.get("koid"), str):
                raise SchemaError("semantic_target.plan.steps: resolve_entity needs koid")
        else:  # project
            if not isinstance(step.get("properties"), list):
                raise SchemaError("semantic_target.plan.steps: project needs properties")

    policy = example["policy"]
    if policy["authorization_required"]:
        for key in ("subject", "action"):
            if not isinstance(policy.get(key), str) or not policy[key].strip():
                raise SchemaError(
                    f"policy: authorization_required demands subject and action "
                    f"(missing {key})")
    for entry in example["context"]["evidence"]:
        if not isinstance(entry, dict):
            raise SchemaError("context.evidence: entries must be objects")
    try:
        _dump(example)  # a dataset is JSONL — non-serializable content is invalid
    except (TypeError, ValueError) as exc:
        raise SchemaError(f"not JSON-serializable: {exc}")

    expected = compute_id(example)
    if example["example_id"] != expected:
        raise SchemaError(
            f"example_id does not match content: {example['example_id']!r} != {expected!r}")
