# AIKOQL vs competitors — benchmark report (P5-M14 supplement)

**2026-09-16** · **re-stamped 2026-09-15** · **M17 scale re-stamp 2026-09-16 (`3152c5e`)** · **CI-07 hybrid-workload re-stamp 2026-09-25** · **CI-08 reproducible-results re-stamp 2026-09-25** · **macOS ARM re-stamp 2026-11-02 (`5bf9739`) — ClickHouse column added** · results: [`result.json`](result.json) + [`result.csv`](result.csv) · harness: `scripts/competitor_bench/bench.py` + [`scale.py`](../../../scripts/competitor_bench/scale.py) · measured at commit `c4931ed`, re-measured at `dc42b16` (release SDK build)

This is a **published report, not a CI gate** (the ND-14 acceptance: competitor
comparisons ship as reports). Same deterministic dataset in six engines (CI-07
added the composed MongoDB stack; the 2026-11-02 stamp adds ClickHouse as the
OLAP-shaped column — Prelude-style device-identity providers run on
columnar stores), same workload cells on every side, every cell
oracle-checked (`correct: true` for all cells of this stamp).

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

## P5-M17b re-stamp (focused acceptance, 2026-09-15, commit `6873bcc`)

M17b powered the index end-to-end: SDK `create_index` / MCP `index_create`,
production maintenance in both hosts, stats behind the declaration, and two
O(store)-per-query costs removed (idx.verify's walk → per-index applied-seq
stamp with an O(1) short-circuit; statistics()'s catalog heads walk → a
per-kernel parsed-stats cache, still watermark-judged). The guard chain is
byte-identical to M9/M15 — lagging or stale indexes still fall back to
FullScan, pinned by cbo_default_002/005.

Focused acceptance run (same harness path, N=1 000, n=50, oracle ok on both
cells — not a full 4-engine re-stamp; that lands with the M17 scale run):

| cell | aikoql (this stamp) | aikoql (pre-17b, dc42b16) | PostgreSQL |
|---|---|---|---|
| structured_filter | **9.11 / 18.17** ms warm p50/p95 | 19.9 / 23.6 | 2.10 / 2.74 |
| point_read | **0.010 / 0.010** | 0.009 / 0.011 | 1.85 / 2.80 |

structured_filter went 19.9 → 9.11 ms warm p50 (2.2×) — the index path is
live. The residual gap to PG's 2.10 ms decomposes honestly (measured via a
pure-Rust probe of the same store shape, 5.55 ms p50 without PyO3):

- **~2–3.5 ms** per-row point reads: the index materializes 500 koids, each
  resolved through `scan_by_type_range`'s readable-object filters (two repo
  reads + payload type check + Deleted skip + ACL authorize) — the M15
  row-for-row parity contract. A bare `raw_object_at` is 0.8 µs/row; the
  filters are the correctness pin, not waste.
- **~3.5 ms** PyO3 marshaling of the 500 returned rows through the SDK
  surface — the harness measures the public SDK, so this is the product.
- **~2 ms** executor filter + cached-object clone passes — pinned by the
  M15 byte-identical-execution contract; shaving them is out of M17b scope.

Batching the point reads was already falsified (SE2-M25: get_many 0.73–1.13×
on warm cache). The honest M17b landing zone is ~2.2× faster than the
pre-17b product and ~4.3× PG on this cell — while point_read remains ~185×
faster than PG (0.010 vs 1.85 ms). The ~2.5 ms aspiration from the plan was
not met; the number above is what the decomposition says is possible without
breaking the pinned contracts.

## P5-M17 — scale-out, the MCP column, and the defect it caught (re-stamp `3152c5e`, 2026-09-16)

M17 extended the harness from the N=1 000 comfort zone to the architecture
questions: 100k/1M scale runs, a multi-op transaction cell against PG, and a
full MCP-mode column (same dataset, same cells, localhost aikoql-mcp). All 22
cells oracle-correct; the run doubles as the M18 decision evidence.

The MCP column caught a real defect. structured_filter over the wire measured
**725 ms warm p50 at N=1 000** — 79× the embedded 9.11 ms for the same store
and the same cell. Bisecting the path showed the frame writer serialized each
JSON-RPC frame through `Display`, which emits one `write()` syscall per JSON
token: 33 008 syscalls for one 127 KB frame, ~700–900 ms per frame on Windows.
RED pinned the contract (one bounded write per frame — the
`write_frame`-bounds-writes test), the fix serializes once and writes once
(`3152c5e`), and the re-run dropped structured_filter to **18.1 ms warm p50
(40×)**. Every other cell in the column dropped with it — the fix is on the
shared write path (point_write 1.75 ms, transactions 1.60 ms, vector_recall
13.4 ms warm p50).

### Scale (embedded, warm p50 ms; all cells oracle-correct)

| cell | 100k | 1M |
|---|---|---|
| point_read | 0.004 | 0.004 |
| point_write | 3.02 | 4.00 |
| structured_filter | 209 | 2 420 |
| transactions | 3.00 | 3.99 |
| graph | 0.013 | 0.018 |
| vector_recall | 0.167 | 0.35 |
| knowledge_query | 2.85 | 6.6 |
| ingest (s) | 1 021 | 10 576 |
| RSS / disk | 1 596 MB / 199 MB | 3 458 MB / 1 733 MB |

This stamp ran on the macOS ARM laptop, not the Windows x64 desktop of the
previous stamps (caveat 2). Point ops, transactions, and graph stay flat from
100k → 1M; the two O(N) cells changed shape because the engine changed:
vector_recall **17 002 → 0.35 ms at 1M (48 000×)** — the AG-20 filtered ANN +
int8 quantization serve the cell; structured_filter **22 912 → 2 420 ms
(9.5×)** — the AG-21 property index serves the match, and 2 420 ms is the
500k-row result transfer the plan's correction names as the floor (the cell
returns half the dataset; no index can make transfer free). The knowledge_query
row is new at scale — the CI-07 five-step pipeline at 6.6 ms warm p50 at 1M.
Point writes (0.63 → 4.00 ms) and ingest (2 139 → 10 576 s) are fsync-bound
machine terms (APFS on this Mac, 16 GB RAM), not engine regressions: the
100k→1M ratios are flat where the paths are sublinear.

**RSS and the AG-20c gate — honestly.** The 1M leg measures
`rss_kb = 3 458 MB` against the 2026-09-16 baseline's 1 005 MB. This stamp
does **not** claim the int8-quantization RSS gate closed: (a) the baseline was
a 6-cell stamp — knowledge_query (added 2026-09-25) keeps fused text+vector
ranking state, so this 7-cell stamp is not cell-for-cell comparable; (b) the
machine moved (macOS ARM 16 GB vs Windows x64 32 GB — allocator retention and
page accounting differ); (c) the harness samples rss after all cells run, with
the 500k-row structured_filter result still materialized in the Python process
— that resident result dwarfs the store's own footprint (the bench's 2-d
vectors quantize to ~6 MB). The gate needs a same-shape, same-machine baseline
before it can be called closed; the quantization's honest measured wins are
the per-cell latencies above.

### Multi-op transaction cell (batch of N in one transaction; warm p50 ms, throughput ops/s)

| batch | aikoql-mcp | PostgreSQL |
|---|---|---|
| 10 | 5.0 — 196.9 op/s | **1.75 — 488.6 op/s** |
| 100 | 15.0 — 63.9 op/s | **12.8 — 76.8 op/s** |

PG wins both batch sizes on this stamp. The old stamp's PG column was the
machine-bound one (its WSL2 recovery incident: 63.6 ms batch 10 with no engine
change) — this stamp's 1.75 ms is the recovered native number. The aikoql side
carries this machine's ~4 ms/commit APFS fsync floor (the same term as the
embedded point_write cell), so every transaction is commit-bound. Both engines
durably commit; the ratio is platform fsync, not protocol.

### M18 decision — SHIP

vector_recall warm p50: 5.0 ms @1k (`dc42b16`) → 1 092 ms @100k → 17 002 ms
@1M. That is 15.6× time for 10× corpus — slightly superlinear, the top-k sort
over the full head set — extrapolating to minutes per query at 10M. Brute
force does not hold; the plan's decision point resolves to **ship an ANN
index**. Two scope notes the measurement adds:

- Attaching the HNSW alone is not enough: the coordinator's vector leg walks
  every head to score, so a capacity-capped candidate set would rank
  non-members as 0.0 above real negative-cosine neighbors ("vmap hole").
  Ranking must become candidate-driven (pinned by the `ann001` RED).
- The gain cell is vector_recall at 100k/1M. The N=1 000 column above
  (13.4 ms over the wire) is write-path-bound, not search-bound.

## CI-07 — the hybrid knowledge workload (2026-09-25)

The launch plan's §10 proposition, benchmarked: ONE flagship
knowledge-query workload that walks the whole pipeline — identity
resolution (natural key → KOID) → metadata filter (topic scope gate) →
traversal (provenance closure) → semantic retrieval (text + vector) →
ranking (RRF fusion) — end-to-end, against the composed stacks, not
microbenchmarks. TESTING-PLAN §5 wires it into the nightly Tier 2
`competitor-matrix` job (`scripts/competitor_bench/bench.py`); the laptop
re-stamps this published report from the same harness.

### The cell, step by step

| step | aikoql | PostgreSQL (composed) | Neo4j | MongoDB (composed) | ClickHouse (composed) |
|---|---|---|---|---|---|
| identity | `MATCH note WHERE seq == N RETURN koid` (by_seq unique index) | `SELECT koid FROM notes WHERE seq = $1` (unique index) | `MATCH (n:Note {seq: $s})` (uniqueness constraint) | `find_one({seq})` (unique index) | `SELECT koid FROM notes WHERE seq = N` (MergeTree ORDER BY koid) |
| filter | `get(anchor).properties.topic` | `SELECT topic … WHERE koid` | `RETURN n.topic` | `find_one({_id}, {topic: 1})` | `SELECT topic … WHERE koid` |
| traversal | `traverse(mentions + derived_from, outbound)` | app walks the indexed edge tables | `-[:MENTIONS]->` / `-[:DERIVED_FROM]->` | app walks the edge collections (indexed) | app walks the edge tables |
| semantic | `find_similar(text="cats", vector, fusion="rrf")` | `LIKE '%cats%'` + `ORDER BY embedding <=> $q::vector` | `CONTAINS 'cats'` + `db.index.vector.queryNodes` | `$regex` + the bolted-on qdrant mirror | `cosineDistance(embedding, q)` + `LIKE '%cats%'` |
| ranking | kernel RRF (k0=60) | app-side RRF (same formula) | app-side RRF | app-side RRF | app-side RRF |

§11 fairness: **qdrant alone carries `no_analog`** — a vector store is not a
knowledge stack. Mongo's native vector search is Atlas-only, so its semantic
leg is the §11-blessed bolted-on composition (the notes mirrored into
qdrant; point ids = note indexes, topic payload for the scope filter). PG's
traversal is the app-side edge-table walk the row's "+app traversal" names,
and its vector leg runs on the pgvector image (the PG column moved from
`postgres:16-alpine` to `pgvector/pgvector:pg16`). The app-side RRF helper
implements the kernel's exact fusion formula (1/(60+rank), 1-indexed,
score > 0 only) so the composed stacks rank with the same math.

ClickHouse carries `no_analog` for the three cells a columnar store has no
native shape for: **point_write, transactions, graph** (its own engine has
point updates and transactions, but the harness's per-op commit + edge-walk
cells are row-store workloads — §11 maps the absent analog honestly instead
of bolting one on). The remaining four cells run natively: point_read and
structured_filter as filtered scans over the MergeTree `ORDER BY koid` sparse
primary index, vector_recall as a full-scan `cosineDistance` ranking
(no ANN index in this stamp — the honest OLAP shape), knowledge_query as the
same five steps with app-side RRF. The column speaks over plain HTTP
(JSONCompact/JSONEachRow, stdlib urllib — no new Python dependency in the
harness).

### Full matrix re-stamp (warm p50 / p95 / p99 ms; ops/s; n=50, all oracle-correct)

| workload | aikoql | PostgreSQL | Neo4j | Qdrant | MongoDB | ClickHouse |
|---|---|---|---|---|---|---|
| point_read | **0.003 / 0.003 / 0.003** — 348 532 | 0.130 / 0.150 / 0.169 — 7 564 | — | — | — | 0.849 / 0.954 / 1.042 — 1 159 |
| point_write | 4.002 / 4.046 / 4.066 — 250 | **0.158 / 0.194 / 0.287** — 6 073 | — | — | — | — |
| structured_filter | 1.431 / 1.572 / 15.637 — 578 | **0.190 / 0.208 / 0.219** — 5 240 | — | — | — | 0.934 / 1.175 / 1.928 — 1 026 |
| transactions | 3.993 / 4.053 / 5.017 — 254 | **0.293 / 0.334 / 0.463** — 3 302 | — | — | — | — |
| graph | **0.007 / 0.010 / 0.010** — 132 231 | — | 0.832 / 1.025 / 1.267 — 1 152 | — | — | — |
| vector_recall | **0.470 / 0.543 / 0.608** — 2 062 | — | — | 0.733 / 0.806 / 0.957 — 1 344 | — | 0.908 / 1.064 / 1.461 — 1 068 |
| knowledge_query | **0.973 / 1.107 / 1.135** — 1 028 | 1.065 / 1.161 / 1.312 — 988 | 9.846 / 12.278 / 13.203 — 100 | — (no_analog) | 3.931 / 4.409 / 4.954 — 256 | 5.791 / 8.212 / 8.494 — 177 |

vector_recall moved 2.19 → 0.470 ms since the CI-07 stamp (4.7× — the AG-20
quantized ANN serves the cell) and aikoql now also wins the flagship
knowledge_query (0.973 vs PG's 1.065 ms warm p50: the whole five-step
pipeline in one process beats the per-step round trips). structured_filter
moved 5.87 → 1.431 ms (4.1×, AG-21) but PG still wins the cell (0.190 ms).
The aikoql point_write/transactions columns are this machine's ~4 ms APFS
fsync floor (caveat 2) — the old stamp's 0.65/0.81 ms ran on NTFS. Footprint
(after ingest + workload run): aikoql 191 MB RSS / 2.7 MB disk / 11.7 s
ingest · PG 33 MB / 65.9 MB / 0.03 s · Neo4j 646 MB / 541.6 MB / 0.79 s ·
Qdrant 342 MB / 1.2 MB / 0.01 s · Mongo 122 MB / 316.5 MB / 0.04 s ·
ClickHouse 1 707 MB / 248.3 MB / 0.03 s.

### What the harness exposed (documented, not hidden)

- **Duplicate-coordinate clusters collapse the ANN.** The original dataset's
  2-d embeddings held 250 identical vectors per class. Exact-duplicate
  clusters are adversarial to HNSW-class graphs — neighbor selection becomes
  insertion-order sensitive, and a re-apply replay (the per-connect by_seq
  re-declaration) reordered inserts until the vector leg returned zero 1.0
  neighbors (probed: 10 × 0.707, no 1.0s). The dataset now carries a seeded
  per-note jitter (±0.048) — the four clusters are distinct points, the
  class geometry is unchanged, and the scalar fields stay byte-identical to
  the M14/M17 runs.
- **`traverse` defaults to both directions.** The kernel's default merge of
  inbound + outbound edges put the anchor's *incoming* derived_from source
  into the reach set; the composed stacks walk outbound edges. The cell
  filters to outbound via the SDK's per-hit `direction` tag.
- Harness robustness, same stamp: the qdrant client timeout is 60 s
  (collection creation on a busy Docker Desktop exceeded the 5 s default),
  the Mongo mirror's point ids are the note indexes (qdrant accepts only
  unsigned-int/UUID ids), and the aikoql store path is handed to the SDK
  nonexistent (the v2 store adopts no existing directory).

## CI-08 — reproducible results + reports (2026-09-25)

The launch plan's §13/§14/§18 proposition: every artifact this report
publishes is machine-checked against one schema contract, the competitor
stacks are version-pinned, and the report ships as a trio.

### §13 — the schema, enforced by `artifact_schema.py`

`python scripts/artifact_schema.py docs/certification/competitors/result.json`
validates the artifact with **named errors** (path + field + §, never a
KeyError) and the freshness stamp (`environment.git_sha` must equal the
checked-out HEAD — a stale artifact can never be re-published silently).
The contract:

- **per engine column** — `cpu_seconds`, `memory_mb`, `disk_bytes`,
  `ingest_s`, and the 7 workload cells; every n>0 cell carries
  p50/p95/p99 + throughput. aikoql's `cpu_seconds` must be measured
  in-process; the container columns may be `null` on runners without
  docker access (GitHub runners cannot probe their service containers —
  the laptop is the canonical measuring host, and the arch gate pins the
  workflow tags instead).
- **environment** — os / cpu / ram_mb / cache_state / harness_sha (plus
  the freshness `git_sha`).
- **config + dataset + seed** — the harness knobs and dataset shape.
- **§18 engine_versions** — aikoql's SDK version and, where the measuring
  host could probe, `{image, digest}` per composed stack.

The CI `competitor-matrix` job runs the same validator on its nightly
artifact, so the nightly evidence satisfies the same contract the laptop
evidence does. The schema pins live in `scripts/test_artifact_schema.py`
(23 pins: 14 original + 9 competitor — happy paths, every §13 omission,
the §18 digest rule, and the main() dispatch).

### §18 — pinned competitor versions

| stack | image | digest (measured) | engine |
|---|---|---|---|
| PostgreSQL (composed) | `pgvector/pgvector:pg16` | `0dd3149a…` | PostgreSQL 16 + pgvector |
| Neo4j | `neo4j:5-community` | `249a1ec6…` | Neo4j 5 |
| Qdrant | `qdrant/qdrant:v1.19.1` | `12364fe8…` | Qdrant 1.19.1 |
| MongoDB (composed) | `mongo:7` | `8bb44427…` | MongoDB 7.0 |
| ClickHouse (composed) | `clickhouse/clickhouse-server:24.8` | `1ffa82ed…` | ClickHouse 24.8 |

No `:latest` anywhere: the workflow's qdrant pin moved to `v1.19.1`
(verified the same digest as what `:latest` pulled), `containers.sh`
carries the same explicit tags (ClickHouse pinned at the 24.8 LTS), and
the arch-gate workflow test 9 fails the gate on any unpinned image in
either file. The digests above are what this laptop pulled — the exact
builds every number in this report belongs to. (Version-tracked tags like
`pg16` still move upstream; the recorded digest pins the measured build
regardless.)

### §13 at this stamp — the resource columns

| engine | cpu_s | mem MB | disk MB | knowledge_query p50 ms |
|---|---|---|---|---|
| aikoql | 2.20 | 191 | 2.7 | 0.97 |
| PostgreSQL | 0.11 | 33 | 65.9 | 1.07 |
| Neo4j | 3.60 | 646 | 541.6 | 9.85 |
| Qdrant | 0.08 | 342 | 1.2 | — (no_analog) |
| MongoDB | 0.14 | 122 | 316.5 | 3.93 |
| ClickHouse | 0.87 | 1 707 | 248.3 | 5.79 |

CPU is the cgroup delta across the engine's bench call (Docker Desktop on
macOS exposes cgroup v2 to the Linux VM; the v1/v2 probe is in
`container_cpu_usec`). Environment at this stamp: macOS 27.0.1 arm64
(Darwin), 16 384 MB RAM, cache_state `fresh-store, warmup-per-cell` —
the previous stamps ran on a Windows x64 desktop with 32 GB, so the
absolute numbers are not comparable across stamps (the ratios and the
100k→1M shapes are).

### §14 — the report trio

`result.json` (the machine-checked artifact), `result.csv` (the flat
engine × workload matrix — one row per cell, both legs uploaded by the
nightly job), and this markdown report. One harness writes all three.

## Method

- **Dataset (seed 42)**: 1 000 notes (`topic` pet/wild, `body` cats/dogs/fish/birds), 500 events, 1 200 `mentions` edges, 200 `derived_from` edges, 2-d embeddings per note (CI-07: seeded per-note jitter — the CI-07 section says why). One dataset generator; each engine loads the same rows/edges/vectors.
- **Cells**: 50 ops per cell, warm = fresh connection + 10 warmup ops. "Cold" = fresh connection, 0 warmup. Per-op oracle: the engine must return the expected row/hit/edge set every op.
- **Engines**: aikoql 0.3.0 (embedded Python SDK, release build, aikoql-v2) · `pgvector/pgvector:pg16` · `neo4j:5-community` · `qdrant/qdrant:v1.19.1` · `mongo:7` · `clickhouse/clickhouse-server:24.8` (all local Docker Desktop containers except the embedded aikoql column).

| workload | aikoql | PostgreSQL | Neo4j | Qdrant | MongoDB | ClickHouse |
|---|---|---|---|---|---|---|
| point_read | `get(koid)` | `SELECT … WHERE koid = $1` (PK) | — | — | — | `SELECT … WHERE koid` (ORDER BY koid) |
| point_write | `remember(koid=…)` update | `UPDATE … SET body` (autocommit) | — | — | — | — |
| structured_filter | `MATCH note WHERE topic == "pet"` | `SELECT … WHERE topic = 'pet'` (indexed) | — | — | — | `SELECT … WHERE topic = 'pet'` |
| transactions | single-op `remember` (one journal commit) | `INSERT` in explicit `BEGIN/COMMIT` | — | — | — | — |
| graph | `traverse(koid, "mentions", 1)` | — | `MATCH (n)-[:MENTIONS]->(e)` | — | — | — |
| vector_recall | `find_similar(vector_only, k=10)` | — | — | `query_points` k=10 | — | `cosineDistance … ORDER BY … LIMIT 10` |
| knowledge_query | the 5-step pipeline (CI-07 section) | composed: SQL + app traversal | Cypher + vector index | — (no_analog) | composed: docs + qdrant mirror | composed: SQL + app RRF |

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
2. **Platform**: macOS 27 ARM, Docker Desktop. Container fsync runs in the
   Docker Desktop Linux VM; the embedded engine's fsync is native APFS. The
   previous stamps' Windows/WSL2 write penalties (40–50 ms PG commits) are
   absent here — PG's write cells are now sub-ms — while aikoql's embedded
   point_write/transactions sit at this machine's ~4 ms/commit APFS fsync
   floor (0.65–0.96 ms on NTFS in the old stamps). Treat every write column
   as platform-bound, not a portable ratio.
3. **Cold ≠ cold cache.** "Cold" = fresh connection. OS page cache stays warm
   from ingest on every side (Windows cannot drop cache unprivileged).
4. **Single run.** Run-to-run variance was observed on this machine (e.g.
   aikoql structured_filter 13.4→21.9 ms across back-to-back runs under the
   same workload). Numbers are one stamped run, like the ND-14 suites.
5. **structured_filter's index refresh is per-connect.** PostgreSQL pays its
   `CREATE INDEX ON notes(topic)` once at ingest, inside the timed ingest
   window. aikoql's harness cell re-declares `create_index` on every fresh
   connection (outside the measured op): the M9 staleness contract makes any
   later write — the write cell runs first — stale the stats, so the
   declaration's rebuild + analyze must run per connect for the optimizer to
   price the index. An embedded SDK kernel resumes its maintainer live at
   open, so steady-state reuse of one connection pays no refresh. The refresh
   cost is disclosed, not hidden: it stays out of the timer and is reported
   in the P5-M17b section above. (Pre-17b this caveat said the machinery was
   unpowered — no declaration surface, no production maintainer, no stats —
   which is exactly what P5-M17b shipped.)
6. **vector_recall is not symmetric either.** Both sides now run HNSW ANN
   indexes (aikoql shipped P5-M18 after this caveat was first written); the
   residual asymmetry is the candidate pool and tuning, plus the
   duplicate-coordinate collapse the CI-07 section documents. The historical
   numbers: Qdrant HNSW vs aikoql brute force at parity at N=1 000 (5.0 vs
   5.3 ms warm p50); brute force at 100k/1M is linear-plus (1 092 ms @100k →
   17 002 ms @1M) — the P5-M17 section resolved the evidence gate to SHIP
   the ANN index, and the CI-07 stamp measures it: 2.2 ms warm p50.
7. **"transactions"** = one durable single-statement commit per op on both
   sides (aikoql: one journal commit + fsync; PG: autocommit update / explicit
   insert commit). Multi-statement transactions are not exercised.

## No-analog table

What one engine has that the others in this lineup do not:

| capability | aikoql | PostgreSQL | Neo4j | Qdrant | MongoDB | ClickHouse |
|---|---|---|---|---|---|---|
| owner-based ACL / roles per subject (kernel, default owner-only) | ✅ | role per user | role per user | API key per deployment | role per user | role per user + row policies |
| provenance & evidence (derivation, sources, confidence per object) | ✅ | app-level | app-level | app-level | app-level | app-level |
| temporal queries (`AS OF` time travel on the journal) | ✅ | app-level | app-level | — | app-level | app-level (ASOF joins) |
| hybrid text+vector fusion (RRF) in one query | ✅ | app-level | app-level | hybrid payload queries | app-level | app-level |
| embedded, zero-server deployment | ✅ | — | — | — | — | ✅ (clickhouse-local) |
| agent identity as first-class query subject | ✅ | — | — | — | — | — |
| SQL ecosystem / joins / mature tooling | — | ✅ | — | — | — | ✅ |
| arbitrary property-graph patterns (Cypher) | rel-type BFS only¹ | — | ✅ | — | — | — |
| ANN vector index (sublinear at scale) | ✅ (M18 HNSW) | via pgvector | via plugins | ✅ | Atlas-only | full-scan cosineDistance (this stamp) |

¹ aikoql `traverse` is rel-type-scoped BFS depth-N; the compiler's `TRAVERSE`
clause covers the same shape. Arbitrary multi-hop patterns are a known gap.

## Reading

- aikoql wins the in-process cells (reads 43×+, graph 100×+), the ANN vector
  cell (0.47 vs 0.73 ms at N=1 000), and now the flagship knowledge_query
  (0.97 vs 1.07 ms warm p50 — the whole five-step pipeline in one process
  beats the per-step round trips: Neo4j 9.8, Mongo 3.9, ClickHouse 5.8).
  PG still wins the indexed-filter cell (0.190 vs 1.431 ms) and the
  point-write cell on this machine (0.158 vs 4.002 ms — APFS fsync, caveat 2).
- Footprint is 20–200× smaller on disk than the server engines.
- The differentiation is the **no-analog table**, not the latency table:
  ACL, provenance, time travel, hybrid fusion and agent identity are kernel
  features the competitors delegate to the application layer — and the
  knowledge_query cell is the one workload that exercises several of them
  at once, in one process, in one query.
