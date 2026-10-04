"""T-01 RED — the canonical training-example schema (design §9 + T-01
recon corrections).

These tests fail against the empty package: aikoql_training.models does
not exist yet. GREEN = models.py with fail-closed validation, canonical
sort-keyed serialization and content-derived example IDs (design §24).
"""

import json

import pytest

from aikoql_training.errors import SchemaError
from aikoql_training.models import SCHEMA_VERSION, compute_id, to_json, validate
from conftest import make_example


def test_valid_example_passes():
    validate(make_example())


def test_missing_required_field_rejected():
    for field in ("example_id", "schema_version", "generator_version",
                  "source", "task", "input", "semantic_target",
                  "query_target", "context", "expected", "policy",
                  "labels", "split_key"):
        example = make_example()
        del example[field]
        with pytest.raises(SchemaError, match=field):
            validate(example)


def test_unknown_top_level_field_rejected():
    example = make_example()
    example["hallucinated"] = 1
    with pytest.raises(SchemaError, match="hallucinated"):
        validate(example)


@pytest.mark.parametrize("section", [
    "source", "task", "input", "semantic_target", "query_target",
    "context", "expected", "policy", "labels",
])
def test_unknown_nested_field_rejected(section):
    example = make_example()
    example[section]["hallucinated"] = 1
    with pytest.raises(SchemaError, match="hallucinated"):
        validate(example)


def test_task_type_is_a_known_enum():
    example = make_example()
    example["task"]["type"] = "hallucinate"
    with pytest.raises(SchemaError, match="hallucinate"):
        validate(example)


@pytest.mark.parametrize("field,value", [
    ("question", ""),                     # input.question non-empty str
    ("question", 42),                     # ... and a str, not an int
])
def test_input_question_typed(field, value):
    example = make_example()
    example["input"][field] = value
    with pytest.raises(SchemaError):
        validate(example)


def test_query_target_must_be_text_aikoql():
    # Recon correction: the public query surface is TEXT aikoql
    # (mcp tools/query.rs tool_aikoql → aikoql_compiler::parser::parse),
    # not the design doc's "aikoql-json" payload.
    example = make_example()
    example["query_target"]["language"] = "aikoql-json"
    with pytest.raises(SchemaError, match="aikoql"):
        validate(example)
    example = make_example()
    example["query_target"]["query"] = ""
    with pytest.raises(SchemaError):
        validate(example)


def test_typed_collections_enforced():
    example = make_example()
    example["task"]["requires"] = "graph_traversal"   # must be a list
    with pytest.raises(SchemaError, match="requires"):
        validate(example)
    example = make_example()
    example["expected"]["koids"] = "not-a-list"
    with pytest.raises(SchemaError, match="koids"):
        validate(example)


def test_boolean_fields_enforced():
    example = make_example()
    example["policy"]["authorization_required"] = "yes"
    with pytest.raises(SchemaError, match="authorization_required"):
        validate(example)
    example = make_example()
    example["labels"]["grounded"] = 1
    with pytest.raises(SchemaError, match="grounded"):
        validate(example)


def test_serialization_is_canonical_and_stable():
    example = make_example()
    blob = to_json(example)
    assert to_json(make_example()) == blob            # stable across builds
    assert json.loads(blob) == example                # lossless round-trip
    # sort-keyed: the first nested key alphabetically is "context".
    assert blob.index('"context"') < blob.index('"example_id"')
    assert "\n" not in blob


def test_example_id_is_content_derived_and_stable():
    a = make_example()
    b = make_example()                                # independently built
    assert a["example_id"] == b["example_id"]
    assert a["example_id"].startswith("sha256:")
    assert len(a["example_id"]) == len("sha256:") + 64
    c = make_example()
    c["input"]["question"] = "Who owns the settlement service?"  # reword
    c["example_id"] = compute_id(c)
    assert c["example_id"] != a["example_id"]         # content change moves id
    d = make_example()
    d["source"]["scenario_id"] = "factual:policy:p-02"
    d["example_id"] = compute_id(d)
    assert d["example_id"] != a["example_id"]         # scenario change moves id


def test_embedded_example_id_must_match_content():
    example = make_example()
    example["example_id"] = "sha256:" + "f" * 64      # forged id
    with pytest.raises(SchemaError, match="example_id"):
        validate(example)


# --- T-17 schema v2 (PR #9 review P0.2 / TDD-01): the logical
# knowledge plan is a first-class training artifact. GREEN = models.py
# SCHEMA_VERSION 2 whose semantic_target carries operation + intent +
# entities + requirements + plan, and whose policy demands subject/
# action exactly when authorization_required is true. ---


def test_semantic_target_requires_a_plan():
    example = make_example()
    del example["semantic_target"]["plan"]
    with pytest.raises(SchemaError, match="plan"):
        validate(example)


def test_plan_step_ops_are_closed():
    example = make_example()
    example["semantic_target"]["plan"]["steps"][0]["op"] = "explode"
    with pytest.raises(SchemaError, match="explode"):
        validate(example)


def test_entity_roles_are_closed():
    example = make_example()
    example["semantic_target"]["entities"][0]["role"] = "sidekick"
    with pytest.raises(SchemaError, match="sidekick"):
        validate(example)


def test_policy_authorization_demands_subject_and_action():
    example = make_example()
    example["policy"] = {"authorization_required": True}
    with pytest.raises(SchemaError, match="authorization"):
        validate(example)


def test_policy_authorization_demands_full_decision_metadata():
    # TDD-01 (T-25): authorization_required demands the complete
    # machine-readable verdict — subject, action, resource, the kernel
    # decision and the preserved reason — or the example is invalid.
    base = {"authorization_required": True, "subject": "reader",
            "action": "read", "resource": "service",
            "decision": True, "reason": "allowed"}
    for missing in ("subject", "action", "resource", "decision", "reason"):
        example = make_example()
        policy = dict(base)
        del policy[missing]
        example["policy"] = policy
        with pytest.raises(SchemaError, match="authorization"):
            validate(example)
    example = make_example()
    example["policy"] = dict(base, decision="yes")
    with pytest.raises(SchemaError, match="decision"):
        validate(example)


def test_context_evidence_entries_must_be_dicts():
    example = make_example()
    example["context"]["evidence"] = [
        {"fact": "aikoql", "text": "MATCH (x:Service)"},
        "not-a-dict",
    ]
    with pytest.raises(SchemaError, match="evidence"):
        validate(example)
