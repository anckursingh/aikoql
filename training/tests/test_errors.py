"""T-13 RED: the typed error model (design §28).

Every aikoql-training error carries four optional category fields —
stage (the pipeline stage that raised), scenario (the scenario_id),
code (a stable machine-readable code) and example_id — and renders
them as a JSON-serializable info dict. Pipeline raises use them:
the oracle raise carries stage/code/scenario, the split-leakage
raise carries code. The categories are optional at construction
(the 25 model/schema raises carry none) and inherited by the
subclasses.

Every test below fails against the current tree:
`TrainingDataError` takes no category keyword arguments.
"""

from __future__ import annotations

import json

import pytest

from aikoql_training.errors import DatasetError, SchemaError, TrainingDataError


def test_error_categories_are_preserved():
    err = TrainingDataError("boom", stage="oracle", scenario="sc-1",
                            code="oracle_failed", example_id="ex-1")
    assert err.stage == "oracle"
    assert err.scenario == "sc-1"
    assert err.code == "oracle_failed"
    assert err.example_id == "ex-1"


def test_error_categories_are_optional():
    err = TrainingDataError("plain")
    assert err.stage is None
    assert err.code is None


def test_to_info_renders_all_categories():
    err = TrainingDataError("boom", stage="split", code="split_leakage")
    assert err.to_info() == {
        "message": "boom", "stage": "split", "scenario": None,
        "code": "split_leakage", "example_id": None,
    }


def test_to_info_is_json_serializable():
    err = TrainingDataError("boom", stage="oracle", scenario="sc-1",
                            code="oracle_failed", example_id="ex-1")
    json.dumps(err.to_info())  # raises on failure


def test_subclasses_inherit_the_categories():
    for cls in (SchemaError, DatasetError):
        err = cls("bad", stage="schema", code="bad_field")
        assert err.stage == "schema"
        assert err.code == "bad_field"
        assert err.to_info()["message"] == "bad"


def test_pipeline_raises_carry_stage_and_code(monkeypatch, mcp_server):
    """The oracle raise carries stage/code/scenario; the pipeline
    surface is still one TrainingDataError (the CLI contract)."""
    host, token = mcp_server
    from types import SimpleNamespace
    from aikoql_training import cli
    from aikoql_training.errors import TrainingDataError

    def _fail(db, scenario, queries):
        return {"ok": False, "errors": ["boom"]}

    monkeypatch.setattr(cli, "verify_scenario", _fail)
    args = SimpleNamespace(db=host, token=token, out="unused",
                           config=None, seed=0, database_id="acmepay",
                           metrics=None)
    with pytest.raises(TrainingDataError) as e:
        cli.cmd_generate(args)
    assert e.value.stage == "oracle"
    assert e.value.code == "oracle_failed"
    assert e.value.scenario is not None
