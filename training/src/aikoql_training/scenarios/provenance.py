"""Provenance scenario generator (design Phase 7): examples pointing at
REAL evidence.

KO A with scalar property X and compiled-context evidence rows E ->
"What evidence supports that the X of A is <value>?" -> the citation of
E (document, extractor, page). One scenario per scalar property whose
KO carries citable evidence (document_id + extractor, both non-blank
strings); the evidence tuple is stored verbatim on the scenario so the
answer generator can prove each cited entry appears in the compiled
context. KOs without citable evidence are skipped — nothing is
fabricated (the design's evidence-ID acceptance).

T-08 recon: evidence has TWO real shapes on the two surfaces. The
kernel stores canonical evidence (source_artifact/method/location/
revision — kom.rs `evidence()`, surfaced by trace); the compiled
context carries the IR Evidence rows (document_id/page/source/
extractor/model/confidence — ir.rs, serialized with nulls). An
accepted example's evidence_ids must trace to context rows (T-07's
grounding), so the generator cites the COMPILED shape; canonical
entries are skipped as uncitable rather than cited into a context
that can never carry them (fail-closed seam, pinned by
test_non_citable_evidence_skipped).
"""

from __future__ import annotations

from typing import Any, Dict, List, Tuple

from aikoql_training.scenarios.factual import _eligible
from aikoql_training.scenarios.scenario import Scenario
from aikoql_training.scenarios.templates import ref_of
from aikoql_training.validation.grounding import evidence_id


def _citation(ev: Dict[str, Any]) -> str:
    cite = f"{ev['document_id']} ({ev['extractor']})"
    if ev.get("page") is not None:
        cite += f" p.{ev['page']}"
    return cite


def _citable(ev: Any) -> bool:
    return (
        isinstance(ev, dict)
        and isinstance(ev.get("document_id"), str)
        and ev["document_id"].strip()
        and isinstance(ev.get("extractor"), str)
        and ev["extractor"].strip()
    )


def provenance_scenarios(kos: List[Dict[str, Any]]) -> List[Scenario]:
    scenarios: List[Scenario] = []
    for ko in sorted(kos, key=lambda k: k["koid"]):
        evidence: List[Dict[str, Any]] = []
        seen = set()
        for e in ko.get("evidence", []):
            if not _citable(e):
                continue
            key = evidence_id(e)
            if key in seen:
                continue
            seen.add(key)
            evidence.append(e)
        if not evidence:
            continue
        for prop in sorted(ko["properties"]):
            value = ko["properties"][prop]
            if not _eligible(value):
                continue
            citations = " ; ".join(_citation(e) for e in evidence)
            ref = ref_of(ko["koid"], {ko["koid"]: ko})
            scenarios.append(Scenario(
                scenario_id=f"provenance:{ko['type_name']}:{prop}:{ko['koid']}",
                task_type="provenance",
                difficulty="factual",
                question=(
                    f"What evidence supports that the {prop} of {ref} "
                    f"is {str(value)}?"
                ),
                expected_answer=citations,
                koids=(ko["koid"],),
                property=prop,
                evidence=tuple(evidence),
            ))
    return scenarios
