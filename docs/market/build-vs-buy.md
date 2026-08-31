# Build-vs-Buy (W31-DEV-001, spec §10 row "lower developer complexity")

## Why the Wave 3 number was wrong

Wave 3 compared a 1,042-LOC retrieval baseline against AIKOQL's
9,410-LOC engine surface — a number that cannot prove developer
productivity either way. The spec's correction: build **two equivalent
applications** over the same scenario and measure only
**application-owned** LOC per capability, engine-internal LOC excluded
by construction.

## Method (pinned in `wave31_dev.rs`)

- Same scenario for both apps: deployment-window conflict, retry
  timeline, day-7 capacity supersession and FTP retirement.
- Same scripted agent for both (treatment-neutral, the REAL-001
  convention), same parity battery (6 probes, day 1 + day 7), each
  probe asserted per-outcome for **both** apps — functional parity
  first, LOC second.
- Conventional app: Postgres + vector store + Graph/RAG + custom
  ingestion/temporal/provenance/conflict/memory code, all inline and
  exercised. AIKOQL app: kernel ops + compile + the shared agent
  policy.
- LOC = source-span counts (`line!()` windows owned by each
  capability), so any line added to either app grows its own count.
  Shared test utilities (tokenizer, agent policy) are excluded from
  both. The AIKOQL app's wrapper bookkeeping is counted — its rows are
  upper bounds.
- Acceptance written before first measurement: per-row moat
  (retrieval/provenance/conflict/memory/config), infra components
  1 < 8, total strictly less. Rows may never be padded to win.

## Measured (GREEN, 2026-08-29)

| Capability | Conventional (LOC) | AIKOQL app (LOC) |
|---|---|---|
| configuration | 11 | 9 |
| infrastructure components | **8** | **1** |
| ingestion | 16 | 6 |
| retrieval (rank/pack + orchestration) | 52 | 2 |
| **temporal** | **23** | **65** |
| provenance | 8 | 0 |
| conflict handling | 16 | 0 |
| memory | 19 | 5 |
| **total** | **153** | **88** |

Parity battery: 6/6 probes green for both applications (day-1 retry
supersession with old versions forbidden, day-7 capacity flip with the
old value forbidden, FTP tombstone → Refuse, deployment-window
conflict pair complete).

## What the moat is — and is not

- **Moat (aikoql strictly less):** retrieval (2 vs 52 — one
  validity-bounded compile), provenance (0 vs 8 — kernel-managed),
  conflict (0 vs 16 — the conflict registry is a kernel claim, the
  day-1 conflict handler's load-bearing behavior is micro-asserted on
  the conventional side), memory (5 vs 19), configuration (1 component
  vs 8). Total 88 vs 153.
- **Measured loss, kept (losses.md):** temporal bookkeeping costs more
  app code on the AIKOQL side (65 vs 23) — nine lineage operations
  through the kernel API against chunk replacement + transcript scrub.
- **Near parity, kept:** the ops proxy for one knowledge-rule change
  is 4 statements (find claim, supersede, update list, refresh stale
  set) vs 3 conventional (remove chunk, insert successor, scrub
  transcript), one callsite each.

## What this experiment deliberately does not claim

- Developer hours, defect counts, time-to-add-source,
  time-to-change-rule: printed **n/a** — human measurements a
  deterministic CI cannot fake. Deterministic ops proxies are
  printed instead (add source: 1 ingest call both sides; rule change:
  above).
- Any engine-internal complexity comparison. The 9,410-LOC engine
  figure is not referenced by this experiment.
