"""FZ-T1 — the canonical-schema validator under arbitrary JSON.

Properties (corpus pinned once GREEN lands, SDK fuzz-estate pattern):
  1. validate() either raises SchemaError or accepts — it never panics on
     arbitrary JSON, and a non-dict input is always a SchemaError.
  2. An accepted dict round-trips through to_json byte-identically and
     re-validates.
  3. A valid example stays valid under arbitrary question text and
     arbitrary context rows — schema-level validation does not inspect
     content.
"""

import json

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from aikoql_training.errors import SchemaError
from aikoql_training.models import to_json, validate
from conftest import make_example

_json = st.recursive(
    st.none() | st.booleans() | st.integers() | st.floats(allow_nan=False)
    | st.text(max_size=200) | st.binary(max_size=50),
    lambda children: st.lists(children, max_size=8)
    | st.dictionaries(st.text(max_size=40), children, max_size=8),
    max_leaves=40,
)

# Context rows are KO-shaped data: the JSON-safe domain of a JSONL schema
# (non-serializable content is invalid by construction, so "stays valid"
# only applies to JSON-safe rows).
_json_safe = st.recursive(
    st.none() | st.booleans() | st.integers() | st.floats(allow_nan=False)
    | st.text(max_size=200),
    lambda children: st.lists(children, max_size=8)
    | st.dictionaries(st.text(max_size=40), children, max_size=8),
    max_leaves=40,
)


@given(_json)
@settings(max_examples=200)
def test_arbitrary_json_fails_closed_or_validates(value):
    if not isinstance(value, dict):
        with pytest.raises(SchemaError):
            validate(value)
        return
    try:
        validate(value)
    except SchemaError:
        return
    # Accepted ⇒ canonical serialization works and re-validation agrees.
    blob = to_json(value)
    assert json.loads(blob) == value
    validate(value)


@given(
    question=st.text(min_size=1, max_size=200).filter(lambda q: q.strip()),
    context_rows=st.lists(
        st.dictionaries(st.text(max_size=40), _json_safe, max_size=8), max_size=5
    ),
)
@settings(max_examples=100)
def test_valid_example_stays_valid_and_round_trips(question, context_rows):
    example = make_example(
        input={"question": question},
        context={
            "entities": context_rows,
            "facts": [],
            "relations": [],
            "evidence": [],
        },
    )
    validate(example)
    assert to_json(example) == to_json(json.loads(to_json(example)))
