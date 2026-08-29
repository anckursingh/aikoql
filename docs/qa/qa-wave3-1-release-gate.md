# Wave 3.1 Release Gate (spec §9)

The spec's APPROVED rule, evaluated clause by clause. Each clause is
verifiable from `qa-wave3-1-results.json` + the evidence docs cited
there; nothing here is asserted without a pointer.

## Gate evaluation

| Clause | Result | Evidence |
|---|---|---|
| ALL P0 PASS | **PASS** — 13/13 | MKT/COMP/REAL/DEC/TEMP/UNK/MEM/DEV/COST/REPRO/BIAS/NEG/REG, each pinned by its wave31 test + evidence doc |
| 0 Sev-1 | **PASS** | No Sev-1 open. Wave 2 certify chain GO; no correctness/security/knowledge-integrity blocker found in Wave 3.1 |
| 0 Sev-2 | **PASS** | No Sev-2 open. Known losses are measured ceiling rows (losses.md), not blockers |
| Wave 2 = GO | **PASS** | certify.js chain: MVP GO → Wave 2 GO → Wave 3 GO (artifacts/, `node scripts/certify.js --check` green) |
| ≥1 meaningful workload class shows repeatable advantage | **PASS** | COMP-001: 9 strong-fit classes, 0 regression classes. REPRO-001: two independent passes, identical on all mechanical columns |
| build-vs-buy evidence methodologically valid | **PASS** | DEV-001: two equivalent apps, application-owned LOC per capability, engine LOC excluded by construction, moat asserted per row (green pins) |
| headline results reproducible | **PASS** | REPRO-001 + frozen corpus (corpus-version.md v1.0) + frozen judge + frozen recipe (reproduction.md) |
| negative evidence preserved | **PASS** | losses.md rows kept and cited; NEG-001 four mandated scenarios, 3 no-advantage kept; certification claim-word ban holds (W3-G05) |
| public claims scoped to evidence | **PASS** | public-claims.md: every claim carries its class scope + the denied/kept rows; the universal cost-leadership claim is explicitly DENIED (COST-001 gate) |

## Verdict

**APPROVED** per spec §9, with the spec's own framing restated:

> Approval does not mean AIKOQL is universally better than RAG. It
> means the release has sufficient evidence for scoped, reproducible
> product claims.

## Honest remain-open rows (do not block APPROVED, kept for the record)

- Real-LLM leg of REAL-001 is env-gated (no key on the measurement
  machine) — the deterministic sim leg is green, the live leg prints
  totals without assertions.
- OSS human wall-clock target deliberately not set (no fresh
  developer available) — mechanical 7/7 legs measured instead.
- 1M-row scale criterion is a pointer, not a run (100k run measured).
- Unknown-probe false confidence is not zero (losses.md W11 row).
