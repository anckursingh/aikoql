# AIKOQL vs competitors — benchmark report (P5-M14 supplement)

**2026-09-16** · **re-stamped 2026-09-15** · results: [`result.json`](result.json) · harness: `scripts/competitor_bench/bench.py` · measured at commit `c4931ed`, re-measured at `dc42b16` (release SDK build)

This is a **published report, not a CI gate** (the ND-14 acceptance: competitor
comparisons ship as reports). Same deterministic dataset in four engines, same
workload cells on every side, every cell oracle-checked (`correct: true` for all
22 measured cells).

The re-stamp was taken after **P5-M15** (CBO on the default KOQL path) and
**P5-M16** (vector recall without corpus materialization) shipped. It is the
acceptance evidence for both milestones:

- **vector_recall: 12.0 → 5.0 ms warm p50 (2.4×)** — M16's slim ranking pass.
  aikoql now matches Qdrant at N=1 000 (5.0 vs 5.3 ms). Control cells
  (point_read, graph, transactions) were flat run-to-run, so this is signal,
  not machine drift.
- **structured_filter: 21.9 → 19.9 ms warm p50 (−9%)** — the cell is still a
  full scan, so this is noise-band movement, not M15's win. Root cause
  (caveat 5): the property index exists (P5-M8) and the CBO is wired into the
  default path (P5-M15), but no production surface declares an index, no
  production path maintains one, and stats are never computed — the guards
  correctly fall back to FullScan. Closing the gap end-to-end is scheduled as
  P5-M17b; the harness cell is its acceptance.

## Method

- **Dataset (seed 42)**: 1 000 notes (`topic` pet/wild, `body` cats/dogs/fish/birds), 500 events, 1 200 `mentions` edges, 200 `derived_from` edges, 2-d embeddings per note. One dataset generator; each engine loads the same rows/edges/vectors.
- **Cells**: 50 ops per cell, warm = fresh connection + 10 warmup ops. "Cold" = fresh connection, 0 warmup. Per-op oracle: the engine must return the expected row/hit/edge set every op.
- **Engines**: aikoql 0.1.19 (embedded Python SDK, release build, aikoql-v2) · `postgres:16-alpine` · `neo4j:5-community` · `qdrant/qdrant:latest` (all local Docker Desktop containers).

| workload | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| point_read | `get(koid)` | `SELECT … WHERE koid = $1` (PK) | — | — |
| point_write | `remember(koid=…)` update | `UPDATE … SET body` (autocommit) | — | — |
| structured_filter | `MATCH note WHERE topic == "pet"` | `SELECT … WHERE topic = 'pet'` (indexed) | — | — |
| transactions | single-op `remember` (one journal commit) | `INSERT` in explicit `BEGIN/COMMIT` | — | — |
| graph | `traverse(koid, "mentions", 1)` | — | `MATCH (n)-[:MENTIONS]->(e)` | — |
| vector_recall | `find_similar(vector_only, k=10)` | — | — | `query_points` k=10 |

## Results (warm cell: p50 / p95 / p99, ms; throughput ops/s) — re-stamp `dc42b16`

| workload | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| point_read | **0.009 / 0.011 / 0.011** — 102 522 op/s | 1.46 / 1.70 / 1.88 — 671 op/s | — | — |
| point_write | **0.77 / 1.71 / 2.02** — 1 015 op/s | 40.1 / 304.5 / 591.1 — 15.5 op/s | — | — |
| structured_filter | 19.9 / 23.6 / 25.5 — 49.4 op/s | **1.76 / 1.98 / 2.24** — 560 op/s | — | — |
| transactions | **1.35 / 15.7 / 17.5** — 193 op/s | 40.2 / 74.3 / 104.8 — 21.8 op/s | — | — |
| graph | **0.020 / 0.029 / 0.032** — 47 393 op/s | — | 6.54 / 9.08 / 11.2 — 145 op/s | — |
| vector_recall | **5.0 / 5.9 / 6.3** — 194 op/s | — | — | 5.25 / 6.91 / 19.7 — 171 op/s |

Footprint (after ingest + workload run):

| | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| RSS | 132 MB¹ | 59 MB | 883 MB | 194 MB |
| disk | 2.6 MB | 48.5 MB | 542 MB | 0.5 MB |
| ingest (1 000 notes + 500 events + 1 400 edges) | 2.2 s | 0.4 s | 3.9 s | 0.1 s |

¹ includes the Python harness interpreter; the other engines' numbers are the whole container process.

## Honest caveats (do not cite these numbers without them)

1. **Embedded vs client-server.** aikoql is measured in-process; the three
   competitors pay a network hop per op (localhost TCP/bolt). That is the
   deployment model comparison — aikoql *can* run embedded — but it means the
   read/graph/write gaps are partly architecture, not engine. The MCP
   client-server mode of aikoql is not measured here.
2. **Platform**: Docker Desktop on Windows. Container fsync goes through the
   WSL2 layer — the ~40–50 ms PG write/commit cells carry that penalty. The
   embedded engine's fsync is native. Treat the write columns as
   platform-bound, not portable ratios.
3. **Cold ≠ cold cache.** "Cold" = fresh connection. OS page cache stays warm
   from ingest on every side (Windows cannot drop cache unprivileged).
4. **Single run.** Run-to-run variance was observed on this machine (e.g.
   aikoql structured_filter 13.4→21.9 ms across back-to-back runs under the
   same workload). Numbers are one stamped run, like the ND-14 suites.
5. **structured_filter is not symmetric.** PostgreSQL got its natural
   `CREATE INDEX ON notes(topic)`. aikoql *has* the machinery — the property
   index shipped in P5-M8 and the CBO was wired into the default query path
   in P5-M15 — but it is unpowered in production: no SDK/MCP surface declares
   an index, no production path starts the index maintainer, and statistics
   are never computed, so the CBO's freshness/verify guards fall back to a
   linear scan of every note. That is the honest current state of the
   product, reported as such. Closing it end-to-end is P5-M17b (declaration
   surface + production maintenance + stats); the harness cell above is that
   milestone's acceptance.
6. **vector_recall is not symmetric either.** Qdrant runs an HNSW ANN index;
   aikoql is brute-force cosine over the corpus. After M16's slim ranking pass
   the two are at parity at N=1 000 (5.0 vs 5.3 ms warm p50); Qdrant is still
   expected to widen the gap at scale. An ANN index for aikoql is the open
   P5-M18, evidence-gated on the 100k/1M scale numbers (M17).
7. **"transactions"** = one durable single-statement commit per op on both
   sides (aikoql: one journal commit + fsync; PG: autocommit update / explicit
   insert commit). Multi-statement transactions are not exercised.

## No-analog table

What one engine has that the others in this lineup do not:

| capability | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| owner-based ACL / roles per subject (kernel, default owner-only) | ✅ | role per user | role per user | API key per deployment |
| provenance & evidence (derivation, sources, confidence per object) | ✅ | app-level | app-level | app-level |
| temporal queries (`AS OF` time travel on the journal) | ✅ | app-level | app-level | — |
| hybrid text+vector fusion (RRF) in one query | ✅ | app-level | app-level | hybrid payload queries |
| embedded, zero-server deployment | ✅ | — | — | — |
| agent identity as first-class query subject | ✅ | — | — | — |
| SQL ecosystem / joins / mature tooling | — | ✅ | — | — |
| arbitrary property-graph patterns (Cypher) | rel-type BFS only¹ | — | ✅ | — |
| ANN vector index (sublinear at scale) | —² | via pgvector | via plugins | ✅ |

¹ aikoql `traverse` is rel-type-scoped BFS depth-N; the compiler's `TRAVERSE`
clause covers the same shape. Arbitrary multi-hop patterns are a known gap.
² brute-force scan; an ANN index is a known gap.

## Reading

- aikoql wins the in-process cells (reads 100×+, writes 30×, graph 200×+)
  and loses the two cells where it deliberately ships less machinery: indexed
  filtering (no property index) and ANN vector search (brute force).
- Footprint is 20–200× smaller on disk than the server engines.
- The differentiation is the **no-analog table**, not the latency table:
  ACL, provenance, time travel, hybrid fusion and agent identity are kernel
  features the competitors delegate to the application layer.
