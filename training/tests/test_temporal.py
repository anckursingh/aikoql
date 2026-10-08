"""T-08 RED: temporal scenarios (design Phase 6).

The temporal generator emits version questions over REAL version
intervals: for every committed version the question names the version's
own commit month and the expected answer is that version's value —
March ⇒ v1, August ⇒ v2. A later version earns a question only for
properties whose value CHANGED (an unchanged property would repeat the
earlier answer), and a month-label collision (same question, different
answer) is skipped, never emitted.

Every test below fails against the current tree:
`aikoql_training.scenarios.temporal` does not exist.
"""

from __future__ import annotations

import datetime

import aikoql
from hypothesis import given
from hypothesis import strategies as st

from aikoql_training.generators import build_queries
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.temporal import temporal_scenarios
from aikoql_training.validation import verify_scenario


def _ms(iso: str) -> int:
    """Real epoch millis for an ISO instant (UTC)."""
    return int(datetime.datetime.fromisoformat(iso).timestamp() * 1000)


_MAR = _ms("2026-03-15T12:00:00+00:00")
_AUG = _ms("2026-08-15T12:00:00+00:00")
_JAN = _ms("2026-01-10T00:00:00+00:00")
_K = "a" * 32


def _hist(koid, versions, **head):
    return {"koid": koid, "type_name": "service",
            "properties": head, "versions": versions}


def _v(version, commit_ts, **props):
    return {"version": version, "commit_ts": commit_ts, "properties": props}


# -- the March/August acceptance --------------------------------------------

def test_march_question_answers_v1_august_v2():
    hist = _hist(
        _K,
        [_v(1, _MAR, owner="Alpha", tier=1), _v(2, _AUG, owner="Beta", tier=1)],
        owner="Beta", tier=1,
    )
    by_id = {s.scenario_id: s for s in temporal_scenarios([hist])}
    march = by_id[f"temporal:service:owner:{_K}:v1"]
    august = by_id[f"temporal:service:owner:{_K}:v2"]
    assert march.question == f"What was the owner of service {_K[:8]} in March 2026?"
    assert march.expected_answer == "Alpha"
    assert march.as_of == _MAR  # the real commit_ts, not an invented month
    assert august.question == f"What was the owner of service {_K[:8]} in August 2026?"
    assert august.expected_answer == "Beta"
    assert august.as_of == _AUG


def test_unchanged_property_generates_no_later_version_question():
    hist = _hist(
        _K,
        [_v(1, _MAR, owner="Alpha", tier=1), _v(2, _AUG, owner="Beta", tier=1)],
        owner="Beta", tier=1,
    )
    ids = {s.scenario_id for s in temporal_scenarios([hist])}
    # tier did not change: no v2 question (it would repeat v1's answer)
    assert f"temporal:service:tier:{_K}:v2" not in ids
    # the first version carries every scalar property
    assert f"temporal:service:tier:{_K}:v1" in ids


def test_first_version_questions_every_scalar_property():
    hist = _hist(_K, [_v(1, _MAR, owner="Alpha", tier=1)], owner="Alpha", tier=1)
    scenarios = temporal_scenarios([hist])
    assert {s.scenario_id.split(":")[2] for s in scenarios} == {"owner", "tier"}
    assert {s.expected_answer for s in scenarios} == {"Alpha", "1"}


def test_question_month_derives_from_real_commit_ts():
    hist = _hist(_K, [_v(1, _JAN, owner="Alpha")], owner="Alpha")
    s = temporal_scenarios([hist])[0]
    assert s.question.endswith("in January 2026?")


def test_same_month_collision_is_skipped():
    # Both versions commit in March: the later value would ask the SAME
    # question with a different answer — a contradiction, never emitted.
    late_mar = _ms("2026-03-20T00:00:00+00:00")
    hist = _hist(
        _K,
        [_v(1, _MAR, owner="Alpha"), _v(2, late_mar, owner="Beta")],
        owner="Beta",
    )
    assert [s.expected_answer for s in temporal_scenarios([hist])] == ["Alpha"]


def test_skips_non_scalar_and_malformed_versions():
    hist = _hist(
        _K,
        [
            _v(1, _MAR, owner="Alpha", nested={"x": 1}),
            {"version": 2, "commit_ts": _AUG},  # no properties: malformed
            _v(3, _AUG, owner="Beta"),
        ],
        owner="Beta",
    )
    ids = {s.scenario_id for s in temporal_scenarios([hist])}
    assert f"temporal:service:owner:{_K}:v1" in ids
    assert f"temporal:service:nested:{_K}:v1" not in ids  # not scalar
    assert f"temporal:service:owner:{_K}:v3" in ids  # changed vs v1


def test_temporal_deterministic():
    h1 = _hist(_K, [_v(1, _MAR, owner="Alpha"), _v(2, _AUG, owner="Beta")],
               owner="Beta")
    h2 = _hist("b" * 32, [_v(1, _MAR, owner="Alpha")], owner="Alpha")
    assert temporal_scenarios([h1, h2]) == temporal_scenarios([h2, h1])


def test_temporal_scenario_shape():
    hist = _hist(_K, [_v(1, _MAR, owner="Alpha")], owner="Alpha")
    s = temporal_scenarios([hist])[0]
    assert s.task_type == "temporal"
    assert s.difficulty == "factual"
    assert s.koids == (_K,)
    assert s.property == "owner"
    assert s.expected_path == ()


# -- the query builder ------------------------------------------------------

def test_build_queries_temporal_as_of():
    ko = {"koid": _K, "type_name": "service", "properties": {"owner": "Beta"}}
    s = Scenario(scenario_id="t", task_type="temporal", difficulty="factual",
                 question="q?", expected_answer="Alpha", koids=(_K,),
                 property="owner", as_of=_MAR)
    assert build_queries(s, [ko]) == [f"MATCH service AS_OF {_MAR} RETURN owner"]


def test_build_queries_temporal_unrepresentable():
    ko = {"koid": _K, "type_name": "service", "properties": {}}
    for bad in (
        Scenario(scenario_id="t", task_type="temporal", difficulty="factual",
                 question="q?", expected_answer="x", koids=(_K,),
                 property="owner", as_of=None),  # no real interval
        Scenario(scenario_id="t", task_type="temporal", difficulty="factual",
                 question="q?", expected_answer="x", koids=(_K,),
                 property="o w", as_of=_MAR),  # property is not an ident
    ):
        assert build_queries(bad, [ko]) == []


# -- the oracle -------------------------------------------------------------

class _FakeDb:
    def __init__(self, envs):
        self._envs = list(envs)

    def aikoql(self, q):
        return self._envs.pop(0)


def _temporal_scenario(answer):
    return Scenario(scenario_id="t", task_type="temporal", difficulty="factual",
                    question="q?", expected_answer=answer, koids=(_K,),
                    property="owner", as_of=_MAR)


def test_temporal_oracle_verifies_reconstructed_value():
    db = _FakeDb([{"results": [{"koid": _K, "properties": {"owner": "Alpha"}}]}])
    report = verify_scenario(db, _temporal_scenario("Alpha"),
                             [f"MATCH service AS_OF {_MAR} RETURN owner"])
    assert report["ok"] is True
    assert report["errors"] == []


def test_temporal_oracle_fails_on_wrong_reconstructed_value():
    db = _FakeDb([{"results": [{"koid": _K, "properties": {"owner": "Beta"}}]}])
    report = verify_scenario(db, _temporal_scenario("Alpha"),
                             [f"MATCH service AS_OF {_MAR} RETURN owner"])
    assert report["ok"] is False


# -- property: answers trace to real version snapshots ----------------------

@st.composite
def _histories(draw):
    @st.composite
    def a_record(draw):
        return {
            "koid": draw(st.text(min_size=8, max_size=8,
                                 alphabet=st.characters(
                                     min_codepoint=ord("a"),
                                     max_codepoint=ord("f")))),
            "type_name": draw(st.text(min_size=1, max_size=10,
                                      alphabet=st.characters(
                                          min_codepoint=ord("a"),
                                          max_codepoint=ord("z")))),
            "properties": draw(st.dictionaries(st.text(max_size=12),
                                               st.text(max_size=20),
                                               max_size=4)),
            "versions": draw(st.lists(
                st.fixed_dictionaries({
                    "version": st.integers(min_value=1, max_value=9),
                    "commit_ts": st.integers(min_value=0, max_value=10**12),
                    "properties": st.dictionaries(st.text(max_size=12),
                                                  st.text(max_size=20),
                                                  max_size=4),
                }),
                min_size=1, max_size=4,
                unique_by=lambda v: v["commit_ts"],  # one version per instant
            )),
        }

    return draw(st.lists(a_record(), min_size=1, max_size=4,
                         unique_by=lambda r: r["koid"]))


@given(histories=_histories())
def test_temporal_answers_trace_to_real_version_snapshots(histories):
    for s in temporal_scenarios(histories):
        record = next(h for h in histories if h["koid"] == s.koids[0])
        snap = next(
            v for v in record["versions"]
            if v["commit_ts"] == s.as_of and isinstance(v["properties"], dict)
        )
        # the answer is the version snapshot's value at the real commit_ts
        assert s.expected_answer == str(snap["properties"][s.property])


# -- live: real version intervals over the wire -----------------------------

def test_live_temporal_versions_over_the_wire(mcp_server):
    """Real version intervals end to end: two remembers on one KOID, the
    real commit_ts from trace, each version's snapshot re-read through
    AS_OF over the wire — then the generated questions execute and the
    oracle verifies the reconstructed values."""
    host, token = mcp_server
    with aikoql.Agent.connect(host, token=token) as db:
        koid = db.remember("service", {"owner": "Alpha", "tier": 1})["koid"]
        db.remember("service", {"owner": "Beta", "tier": 1}, koid=koid)
        # trace is McpClient-surface only; over MCP Agent._backend IS the
        # client (the adapter's _call_tool uses the same path)
        versions = db._backend.trace(koid)["versions"]
        assert len(versions) == 2

        # trace's commit_ts is plain epoch millis since T-43 (the MCP
        # boundary decodes the packed HLC); AS_OF takes millis directly
        def ms(v):
            return v["commit_ts"]

        def snapshot(commit_ts):
            rows = db.aikoql(
                f"MATCH service AS_OF {commit_ts} RETURN *")["results"]
            return next(r for r in rows if r["koid"] == koid)["properties"]

        history = {
            "koid": koid, "type_name": "service",
            "properties": snapshot(ms(versions[-1])),
            "versions": [
                {"version": v["version"], "commit_ts": ms(v),
                 "properties": snapshot(ms(v))}
                for v in versions
            ],
        }
        kos = [{"koid": koid, "type_name": "service",
                "properties": history["properties"]}]
        scenarios = temporal_scenarios([history])
        assert scenarios  # real same-month commits: the v1 questions exist
        for s in scenarios:
            queries = build_queries(s, kos)
            assert queries, s.scenario_id
            for q in queries:
                env = db.aikoql(q)
                assert isinstance(env.get("results"), list), q
            assert verify_scenario(db, s, queries)["ok"], s.scenario_id
