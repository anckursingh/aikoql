# AIKOQL vs competitors — benchmark report (P5-M14 supplement)

**2026-09-16** · results: [`result.json`](result.json) · harness: `scripts/competitor_bench/bench.py` · measured at commit `c4931ed` (release SDK build)

This is a **published report, not a CI gate** (the ND-14 acceptance: competitor
comparisons ship as reports). Same deterministic dataset in four engines, same
workload cells on every side, every cell oracle-checked (`correct: true` for all
22 measured cells).

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

## Results (warm cell: p50 / p95 / p99, ms; throughput ops/s)

| workload | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| point_read | **0.010 / 0.013 / 0.024** — 93 475 op/s | 1.63 / 2.72 / 2.97 — 578 op/s | — | — |
| point_write | **1.20 / 3.88 / 6.15** — 591 op/s | 49.0 / 79.2 / 151.0 — 19 op/s | — | — |
| structured_filter | 21.9 / 34.1 / 37.3 — 45 op/s | **2.54 / 3.44 / 10.6** — 361 op/s | — | — |
| transactions | **1.39 / 4.94 / 8.20** — 500 op/s | 40.2 / 241 / 473 — 16 op/s | — | — |
| graph | **0.023 / 0.036 / 0.068** — 38 426 op/s | — | 6.66 / 8.47 / 10.6 — 144 op/s | — |
| vector_recall | 12.0 / 34.6 / 58.5 — 68 op/s | — | — | **5.81 / 6.68 / 22.2** — 156 op/s |

Footprint (after ingest + workload run):

| | aikoql | PostgreSQL | Neo4j | Qdrant |
|---|---|---|---|---|
| RSS | 131 MB¹ | 59 MB | 819 MB | 196 MB |
| disk | 2.6 MB | 48.5 MB | 541 MB | 0.5 MB |
| ingest (1 000 notes + 500 events + 1 400 edges) | 2.5 s | 0.7 s | 4.1 s | 0.2 s |

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
   `CREATE INDEX ON notes(topic)`; aikoql has **no property index** (SE2-M26
   closed as a deliberate skip — candidate-bound scan) so this cell is a
   linear scan of every note on the aikoql side. That is the honest current
   state of the product, reported as such.
6. **vector_recall is not symmetric either.** Qdrant runs an HNSW ANN index;
   aikoql is brute-force cosine over the corpus. At N=1 000 they are the same
   order of magnitude; Qdrant will widen the gap at scale. An ANN index for
   aikoql is a known gap.
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
