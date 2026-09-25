"""AIKOQL Database 1.0 — competitor benchmark (P5-M14 supplement).

Same deterministic dataset (seed 42) loaded into four engines, same workload
cells measured on each side, same report shape as the ND-14 certification
suites (docs/certification/<suite>/result.json). This is a PUBLISHED REPORT,
not a CI gate.

Engines:
  aikoql    — embedded Python SDK (release build), Agent wrapper, aikoql-v2
  postgresql — pgvector/pgvector:pg16 container, SQL + vector (CI-07)
  neo4j     — neo4j:5-community container, Cypher + vector index (CI-07)
  qdrant    — qdrant/qdrant container, vector search
  mongodb   — mongo:7 container, documents (CI-07)

Workload mapping (cell order is fixed across engines):
  point_read        aikoql get(koid)                  vs SELECT ... WHERE koid
  point_write       aikoql remember(koid=...) update  vs UPDATE ... SET body
  structured_filter aikoql MATCH note WHERE topic     vs SELECT WHERE topic='pet'
  transactions      aikoql remember (single-op txn)   vs INSERT in explicit txn
  graph             aikoql traverse(mentions, 1)      vs MATCH (n)-[:MENTIONS]->(e)
  vector_recall     aikoql find_similar(vector_only)  vs qdrant query_points
  knowledge_query   the CI-07 flagship: one query = identity resolution
                    (natural key -> KOID) -> metadata filter (topic scope)
                    -> traversal (mentions + derived_from provenance) ->
                    semantic retrieval -> ranking (text+vector RRF) —
                    end-to-end on aikoql, composed on the competitor
                    stacks (PG+pgvector+app traversal, Mongo+vector via
                    qdrant, Neo4j+vector). §11 fairness: qdrant alone is
                    a vector store, not a knowledge stack — no_analog.

Known caveats (documented in REPORT.md, not hidden):
  - "cold" = fresh connection, 0 warmup; "warm" = fresh connection + 10
    warmup ops. OS page cache stays warm from ingest on every side (Windows
    cannot drop cache unprivileged; containers likewise).
  - aikoql RSS includes the harness interpreter; container RSS is the whole
    server process.
  - aikoql "transactions" is a single-op remember: one journal commit, one
    fsync — the same single-op surface the db-oltp suite certifies.
"""

import json
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import psutil
from aikoql import Agent

REPO = Path(__file__).resolve().parents[2]
OUT_DIR = REPO / "docs" / "certification" / "competitors"

N_NOTES = 1000
N_EVENTS = 500
N = 50  # ops per cell
WARMUP = 10

QUERY_VEC = [1.0, 0.0]
NAMES = ["point_read", "point_write", "structured_filter",
         "transactions", "graph", "vector_recall", "knowledge_query"]


def rrf_topk(text_ranked, vec_ranked, k=10, k0=60):
    """App-side reciprocal rank fusion — the composed stacks' ranking step.
    Same formula as the kernel's Fusion::Rrf (k0=60, 1-indexed ranks,
    score > 0 only): both lists are pre-ordered rank lists of koids."""
    scores = {}
    for lst in (text_ranked, vec_ranked):
        for rank, koid in enumerate(lst, start=1):
            scores[koid] = scores.get(koid, 0.0) + 1.0 / (k0 + rank)
    return [koid for koid, _ in
            sorted(scores.items(), key=lambda kv: (-kv[1], kv[0]))[:k]]

# ---------------------------------------------------------------- dataset

TOPICS = ["pet", "wild"]
BODIES = [
    ("cats", [1.0, 0.0]),
    ("dogs", [0.0, 1.0]),
    ("fish", [0.70710677, 0.70710677]),
    ("birds", [-1.0, 0.0]),
]


def gen_dataset(n_notes=N_NOTES, n_events=N_EVENTS):
    """Seeded dataset; the M17 scale harness reuses this with n_notes=100k/1M
    (same shapes, same seed — the defaults stay byte-identical to the M14 run)."""
    rng = random.Random(42)
    # CI-07 amendment: the embedding column carries a seeded per-note jitter
    # (±0.048) so the four class clusters are DISTINCT points — exact
    # duplicate-coordinate clusters are adversarial to HNSW-class ANN
    # graphs (the neighbor graph becomes insertion-order sensitive and
    # recall collapses on re-apply replays; observed on the aikoql column).
    # Class geometry is unchanged (cats ~1.0, fish ~0.707, dogs ~0.0,
    # birds ~-1.0) and the inter-class order can never flip. The scalar
    # fields (topic/body/seq) stay byte-identical to the M14/M17 runs.
    notes = [
        {"i": i, "topic": TOPICS[i % 2], "body": BODIES[i % 4][0],
         "vec": [BODIES[i % 4][1][0] + 0.001 * (i % 97 - 48),
                 BODIES[i % 4][1][1] - 0.001 * (i % 97 - 48)]}
        for i in range(n_notes)
    ]
    events = [{"i": j, "label": f"e{j}"} for j in range(n_events)]
    # mentions: note i -> event[i % n_events]; i < 200 gets a second edge.
    mentions = [(i, i % n_events) for i in range(n_notes)] + \
               [(i, (i + 1) % n_events) for i in range(200)]
    # derived_from: note i -> note[n_notes - 1 - i] for i < 200.
    derived = [(i, n_notes - 1 - i) for i in range(200)]
    # workload sampling (separate stream — dataset stays byte-identical)
    rng7 = random.Random(7)
    read_ids = [rng7.randrange(n_notes) for _ in range(N)]
    write_ids = list(range(N))  # bodies of notes 0..49 become cats.v2
    graph_ids = [rng7.randrange(n_notes) for _ in range(N)]
    return {"notes": notes, "events": events, "mentions": mentions,
            "derived": derived, "read_ids": read_ids,
            "write_ids": write_ids, "graph_ids": graph_ids}


# ---------------------------------------------------------------- cells

def pct(lats, p):
    """Nearest-rank percentile."""
    return sorted(lats)[max(0, math.ceil(p / 100 * len(lats)) - 1)]


def measure(op, warmup, n):
    for _ in range(warmup):
        op()
    lats, ok = [], True
    t0 = time.perf_counter_ns()
    for _ in range(n):
        t1 = time.perf_counter_ns()
        res = op()
        t2 = time.perf_counter_ns()
        lats.append((t2 - t1) / 1e6)
        ok = ok and bool(res)
    total_s = (time.perf_counter_ns() - t0) / 1e9
    return {"p50_ms": pct(lats, 50), "p95_ms": pct(lats, 95),
            "p99_ms": pct(lats, 99), "throughput_ops_s": n / total_s,
            "correct": ok}


def cell(connect, close, build_op, n=N):
    """cold = fresh connection, 0 warmup; warm = fresh connection + WARMUP."""
    c = connect()
    try:
        cold = measure(build_op(c), 0, n)
    finally:
        close(c)
    c = connect()
    try:
        warm = measure(build_op(c), WARMUP, n)
    finally:
        close(c)
    return cold, warm


def run_engine(connect, close, builders, n=N):
    """builders[i]: None = no analog, or fn(conn) -> op callable."""
    workloads = []
    for name, build in zip(NAMES, builders):
        if build is None:
            workloads.append({"name": name, "n": 0, "no_analog": True,
                              "correct": True})
            continue
        cold, warm = cell(connect, close, build, n)
        workloads.append({
            "name": name, "n": n, "cold": cold, "warm": warm,
            "p50_ms": warm["p50_ms"], "p95_ms": warm["p95_ms"],
            "p99_ms": warm["p99_ms"],
            "throughput_ops_s": warm["throughput_ops_s"],
            "correct": cold["correct"] and warm["correct"],
        })
    return workloads


# ---------------------------------------------------------------- aikoql

def bench_aikoql(ds, kb, n=N):
    agent = Agent.connect(str(kb))
    note_koids, event_koids = [], []
    t0 = time.perf_counter()
    for note in ds["notes"]:
        r = agent.remember("note", {"topic": note["topic"], "body": note["body"],
                                    "seq": note["i"]},
                           semantic={"embedding": note["vec"],
                                     "embedding_model": "bench-2d"})
        note_koids.append(r["koid"])
    for e in ds["events"]:
        r = agent.remember("event", {"label": e["label"], "seq": e["i"]})
        event_koids.append(r["koid"])
    for a, b in ds["mentions"]:
        agent.relate(note_koids[a], event_koids[b], "mentions")
    for a, b in ds["derived"]:
        agent.relate(note_koids[a], note_koids[b], "derived_from")
    # P5-M17b: PG's CREATE INDEX parity — declared (and analyzed) exactly
    # where PG builds notes_topic_idx, inside the timed ingest window.
    agent.create_index("by_topic", "note", ["topic"])
    # CI-07: by_seq serves the knowledge_query cell's identity resolution
    # (natural key -> KOID) — PG builds its unique seq index in the same
    # timed window.
    agent.create_index("by_seq", "note", ["seq"])
    ingest_s = time.perf_counter() - t0
    agent.close()  # cells open their own kernels (fresh connection)

    cats_set = {note_koids[n["i"]] for n in ds["notes"] if n["body"] == "cats"}
    read_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["read_ids"]]
    write_triples = [(note_koids[i], ds["notes"][i]) for i in ds["write_ids"]]
    graph_expect = {
        note_koids[i]: {event_koids[b] for a, b in ds["mentions"] if a == i}
        for i in ds["graph_ids"]
    }
    # CI-07 oracle: the knowledge-query traversal closure — mentions +
    # derived_from targets for every queried anchor.
    kq_expect = {
        note_koids[i]: ({event_koids[b] for a, b in ds["mentions"] if a == i}
                        | {note_koids[b] for a, b in ds["derived"] if a == i})
        for i in ds["read_ids"]
    }

    def connect():
        return Agent.connect(str(kb))

    def build_read(agent):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            ko = agent.get(koid)
            return (ko["koid"] == koid
                    and ko["properties"]["topic"] == note["topic"])

        return op

    def build_write(agent):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(write_triples)
            r = agent.remember("note", {"topic": note["topic"],
                                        "body": "cats.v2", "seq": note["i"]},
                               koid=koid)
            return r["koid"] == koid and r["version"] >= 2

        return op

    def build_filter(agent):
        # P5-M17b: re-declared per cell connect. The M9 stats contract makes
        # ANY later write (the write cell runs first) stale the stats, so the
        # declaration's rebuild + analyze must run here for the optimizer to
        # price the index. Refresh cost stays OUTSIDE the measured op — PG
        # pays it once at ingest, we pay it per connect (embedded SDK: the
        # maintainer resumes live at open). See REPORT.md.
        agent.create_index("by_topic", "note", ["topic"])

        def op():
            out = agent.aikoql('MATCH note WHERE topic == "pet" RETURN *')
            return len(out) == len(ds["notes"]) // 2

        return op

    def build_txn(agent):
        rng = random.Random(7)

        def op():
            r = agent.remember("note", {"topic": "pet", "body": "txn",
                                        "seq": 9000 + rng.randrange(1000)})
            return "koid" in r

        return op

    def build_graph(agent):
        rng = random.Random(7)

        def op():
            koid, expect = rng.choice(list(graph_expect.items()))
            hits = agent.traverse(koid, "mentions", 1)
            got = {h["koid"] for h in hits}
            return got == expect and len(got) > 0

        return op

    def build_vector(agent):
        def op():
            hits = agent.find_similar(vector=QUERY_VEC, k=10,
                                      fusion="vector_only")
            return (len(hits) == 10
                    and all(h["koid"] in cats_set for h in hits))

        return op

    def build_knowledge_query(agent):
        # CI-07 — the hybrid knowledge-query flagship. One query walks the
        # whole pipeline: identity resolution (seq -> KOID through by_seq)
        # -> metadata filter (the topic scope gate) -> traversal (mentions
        # + derived_from provenance) -> semantic retrieval + ranking (the
        # kernel's own text+vector RRF fusion, k0=60 — the same fuse the
        # H5 hybrid query runs; fusion semantics pinned by the H-suite).
        # The by_seq re-declaration mirrors build_filter: the M9 stats
        # contract makes any later write (the write/txn cells run first)
        # stale the stats, so the declaration's rebuild + analyze runs per
        # connect, outside the measured op.
        agent.create_index("by_seq", "note", ["seq"])
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            # 1. identity resolution
            rows = agent.aikoql(
                f'MATCH note WHERE seq == {note["i"]} RETURN koid')
            anchor = rows[0]["koid"] if rows else None
            # 2. metadata filter — the query's scope gate
            ko = agent.get(anchor) if anchor else None
            topic = ko["properties"]["topic"] if ko else None
            # 3. traversal — the anchor's provenance closure. The kernel's
            # default traverse direction merges inbound + outbound; the
            # composed competitor stacks walk OUTBOUND edges (PG/Mongo
            # indexed edge lookups, Neo4j -[:MENTIONS]->), so the harness
            # filters to outbound for the §11-fair mapping.
            reach = set()
            if topic == "pet":
                reach = {h["koid"] for h in agent.traverse(
                    anchor, "mentions", 1) if h["direction"] == "outbound"}
                reach |= {h["koid"] for h in agent.traverse(
                    anchor, "derived_from", 1)
                    if h["direction"] == "outbound"}
            # 4 + 5. semantic retrieval + ranking — one fused call
            ranked = agent.find_similar(text="cats", vector=QUERY_VEC,
                                        k=10, fusion="rrf")
            ok = (anchor == koid and topic == note["topic"]
                  and len(ranked) == 10
                  and all(h["koid"] in cats_set for h in ranked))
            if topic == "pet":
                ok = ok and reach == kq_expect[koid]
            return ok

        return op

    workloads = run_engine(
        connect, lambda c: c.close(),
        [build_read, build_write, build_filter, build_txn,
         build_graph, build_vector, build_knowledge_query], n)

    rss_kb = psutil.Process(os.getpid()).memory_info().rss // 1024
    disk = sum(f.stat().st_size for f in kb.rglob("*") if f.is_file())
    return {"workloads": workloads, "rss_kb": rss_kb, "disk_bytes": disk,
            "ingest_s": round(ingest_s, 2)}


# ---------------------------------------------------------------- postgresql

import psycopg

PG_DSN = "host=127.0.0.1 port=5433 dbname=bench user=bench password=bench"


def bench_pg(ds):
    conn = psycopg.connect(PG_DSN)
    conn.autocommit = True
    conn.execute("DROP TABLE IF EXISTS notes")
    conn.execute("DROP TABLE IF EXISTS events")
    conn.execute("DROP TABLE IF EXISTS mentions")
    conn.execute("DROP TABLE IF EXISTS derived_from")
    # CI-07: the composed PG stack — seq (the natural key), the pgvector
    # embedding, and the edge tables the application walks for traversal.
    conn.execute("CREATE EXTENSION IF NOT EXISTS vector")
    conn.execute("CREATE TABLE notes (koid text PRIMARY KEY, seq integer, "
                 "topic text, body text, embedding vector(2))")
    conn.execute("CREATE TABLE events (koid text PRIMARY KEY, label text)")
    conn.execute("CREATE TABLE mentions (note_koid text, event_koid text)")
    conn.execute("CREATE TABLE derived_from (note_koid text, "
                 "target_koid text)")
    t0 = time.perf_counter()
    note_koids = [f"pg-note-{n['i']}" for n in ds["notes"]]
    event_koids = [f"pg-event-{e['i']}" for e in ds["events"]]
    with conn.cursor() as cur:
        cur.executemany(
            "INSERT INTO notes (koid, seq, topic, body, embedding) "
            "VALUES (%s, %s, %s, %s, %s::vector)",
            [(k, n["i"], n["topic"], n["body"], json.dumps(n["vec"]))
             for k, n in zip(note_koids, ds["notes"])])
        cur.executemany(
            "INSERT INTO events (koid, label) VALUES (%s, %s)",
            [(k, e["label"]) for k, e in zip(event_koids, ds["events"])])
        cur.executemany(
            "INSERT INTO mentions (note_koid, event_koid) VALUES (%s, %s)",
            [(note_koids[a], event_koids[b]) for a, b in ds["mentions"]])
        cur.executemany(
            "INSERT INTO derived_from (note_koid, target_koid) "
            "VALUES (%s, %s)",
            [(note_koids[a], note_koids[b]) for a, b in ds["derived"]])
    conn.commit()
    conn.execute("CREATE INDEX IF NOT EXISTS notes_topic_idx ON notes (topic)")
    conn.execute("CREATE UNIQUE INDEX notes_seq_idx ON notes (seq)")
    conn.execute("CREATE INDEX mentions_note_idx ON mentions (note_koid)")
    conn.execute("CREATE INDEX derived_note_idx ON derived_from (note_koid)")
    ingest_s = time.perf_counter() - t0
    conn.close()

    read_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["read_ids"]]
    write_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["write_ids"]]
    cats_set = {note_koids[n["i"]] for n in ds["notes"] if n["body"] == "cats"}
    kq_expect = {
        note_koids[i]: ({event_koids[b] for a, b in ds["mentions"] if a == i}
                        | {note_koids[b] for a, b in ds["derived"] if a == i})
        for i in ds["read_ids"]
    }

    def connect():
        c = psycopg.connect(PG_DSN)
        c.autocommit = True
        return c

    def build_read(c):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            row = c.execute("SELECT topic FROM notes WHERE koid = %s",
                            (koid,)).fetchone()
            return row is not None and row[0] == note["topic"]

        return op

    def build_write(c):
        rng = random.Random(7)

        def op():
            koid, _ = rng.choice(write_pairs)
            c.execute("UPDATE notes SET body = %s WHERE koid = %s",
                      ("cats.v2", koid))
            return True

        return op

    def build_filter(c):
        def op():
            rows = c.execute("SELECT koid FROM notes WHERE topic = 'pet'")
            return len(rows.fetchall()) == 500

        return op

    txn_counter = [0]

    def build_txn(c):
        def op():
            txn_counter[0] += 1
            koid = f"pg-txn-{txn_counter[0]}"
            c.execute("BEGIN")
            c.execute("INSERT INTO notes (koid, topic, body) "
                      "VALUES (%s, %s, %s)", (koid, "pet", "txn"))
            c.execute("COMMIT")
            return True

        return op

    def build_knowledge_query(c):
        # CI-07 — the composed PG stack: identity + filter + the vector leg
        # in SQL (pgvector), the traversal walked by the application, the
        # ranking fused by the application (rrf_topk = the kernel's RRF).
        rng = random.Random(7)
        vec = json.dumps(QUERY_VEC)

        def op():
            koid, note = rng.choice(read_pairs)
            # 1. identity resolution — unique-index lookup
            row = c.execute("SELECT koid FROM notes WHERE seq = %s",
                            (note["i"],)).fetchone()
            anchor = row[0] if row else None
            # 2. metadata filter — the query's scope gate
            trow = c.execute("SELECT topic FROM notes WHERE koid = %s",
                             (anchor,)).fetchone() if anchor else None
            topic = trow[0] if trow else None
            # 3. traversal — app-side edge walk over the indexed edge tables
            reach = set()
            if topic == "pet":
                reach = {r[0] for r in c.execute(
                    "SELECT event_koid FROM mentions WHERE note_koid = %s",
                    (anchor,))}
                reach |= {r[0] for r in c.execute(
                    "SELECT target_koid FROM derived_from "
                    "WHERE note_koid = %s", (anchor,))}
            # 4. semantic — pgvector cosine over the pet scope
            vrank = [r[0] for r in c.execute(
                "SELECT koid FROM notes WHERE topic = 'pet' "
                "ORDER BY embedding <=> %s::vector", (vec,))]
            # 5. ranking — app-side RRF over the text + vector legs
            trank = [r[0] for r in c.execute(
                "SELECT koid FROM notes WHERE topic = 'pet' "
                "AND body LIKE '%%cats%%' ORDER BY seq")]
            ranked = rrf_topk(trank, vrank)
            ok = (anchor == koid and topic == note["topic"]
                  and len(ranked) == 10
                  and all(k in cats_set for k in ranked))
            if topic == "pet":
                ok = ok and reach == kq_expect[koid]
            return ok

        return op

    workloads = run_engine(
        connect, lambda c: c.close(),
        [build_read, build_write, build_filter, build_txn, None, None,
         build_knowledge_query])

    return {"workloads": workloads, "rss_kb": docker_rss("bench-pg"),
            "disk_bytes": docker_du("bench-pg", ["/var/lib/postgresql/data"]),
            "ingest_s": round(ingest_s, 2)}


# ---------------------------------------------------------------- neo4j

from neo4j import GraphDatabase

NEO4J_URL = "bolt://127.0.0.1:7687"
NEO4J_AUTH = ("neo4j", "benchmarkpass")


def bench_neo4j(ds):
    driver = GraphDatabase.driver(NEO4J_URL, auth=NEO4J_AUTH)
    t0 = time.perf_counter()
    note_koids = [f"neo-note-{n['i']}" for n in ds["notes"]]
    event_koids = [f"neo-event-{e['i']}" for e in ds["events"]]
    with driver.session() as s:
        s.run("MATCH (n) DETACH DELETE n")
        s.run("UNWIND $rows AS r MERGE (n:Note {koid: r.koid}) "
              "SET n.topic = r.topic, n.body = r.body, n.seq = r.seq, "
              "n.embedding = r.embedding",
              rows=[{"koid": k, "topic": n["topic"], "body": n["body"],
                     "seq": n["i"], "embedding": n["vec"]}
                    for k, n in zip(note_koids, ds["notes"])])
        s.run("UNWIND $rows AS r MERGE (e:Event {koid: r.koid}) "
              "SET e.label = r.label",
              rows=[{"koid": k, "label": e["label"]}
                    for k, e in zip(event_koids, ds["events"])])
        s.run("UNWIND $rows AS r MATCH (n:Note {koid: r.a}) "
              "MATCH (e:Event {koid: r.b}) MERGE (n)-[:MENTIONS]->(e)",
              rows=[{"a": note_koids[a], "b": event_koids[b]}
                    for a, b in ds["mentions"]])
        s.run("UNWIND $rows AS r MATCH (n:Note {koid: r.a}) "
              "MATCH (m:Note {koid: r.b}) MERGE (n)-[:DERIVED_FROM]->(m)",
              rows=[{"a": note_koids[a], "b": note_koids[b]}
                    for a, b in ds["derived"]])
        # CI-07: the unique seq constraint (identity resolution's index)
        # and the built-in vector index (Neo4j 5.13+; the knowledge-query
        # semantic leg) — built in the same timed ingest window as the
        # other engines' indexes.
        s.run("CREATE CONSTRAINT note_seq IF NOT EXISTS "
              "FOR (n:Note) REQUIRE n.seq IS UNIQUE")
        s.run("CREATE VECTOR INDEX note_vec IF NOT EXISTS "
              "FOR (n:Note) ON (n.embedding) OPTIONS {indexConfig: "
              "{`vector.dimensions`: 2, "
              "`vector.similarity_function`: 'cosine'}}")
    ingest_s = time.perf_counter() - t0
    driver.close()

    graph_expect = {
        note_koids[i]: {event_koids[b] for a, b in ds["mentions"] if a == i}
        for i in ds["graph_ids"]
    }
    cats_set = {note_koids[n["i"]] for n in ds["notes"] if n["body"] == "cats"}
    read_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["read_ids"]]
    kq_expect = {
        note_koids[i]: ({event_koids[b] for a, b in ds["mentions"] if a == i}
                        | {note_koids[b] for a, b in ds["derived"] if a == i})
        for i in ds["read_ids"]
    }

    def connect():
        return GraphDatabase.driver(NEO4J_URL, auth=NEO4J_AUTH)

    def build_graph(d):
        rng = random.Random(7)

        def op():
            koid, expect = rng.choice(list(graph_expect.items()))
            with d.session() as s:
                recs = s.run("MATCH (n:Note {koid: $k})-[:MENTIONS]->(e:Event) "
                             "RETURN e.koid AS koid", k=koid)
                got = {r["koid"] for r in recs}
            return got == expect and len(got) > 0

        return op

    def build_knowledge_query(d):
        # CI-07 — the composed Neo4j stack: identity + filter + traversal
        # natively in Cypher (its home turf, §11), the semantic leg on the
        # built-in vector index, the ranking fused by the application
        # (rrf_topk = the kernel's RRF).
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            with d.session() as s:
                # 1. identity resolution — unique-constraint lookup
                recs = list(s.run("MATCH (n:Note {seq: $s}) "
                                  "RETURN n.koid AS k", s=note["i"]))
                anchor = recs[0]["k"] if recs else None
                # 2. metadata filter — the query's scope gate
                trow = list(s.run("MATCH (n:Note {koid: $k}) "
                                  "RETURN n.topic AS t", k=anchor))
                topic = trow[0]["t"] if trow else None
                # 3. traversal — the anchor's provenance closure
                reach = set()
                if topic == "pet":
                    reach = {r["k"] for r in s.run(
                        "MATCH (n:Note {koid: $k})-[:MENTIONS]->(e:Event) "
                        "RETURN e.koid AS k", k=anchor)}
                    reach |= {r["k"] for r in s.run(
                        "MATCH (n:Note {koid: $k})-[:DERIVED_FROM]->(m:Note) "
                        "RETURN m.koid AS k", k=anchor)}
                # 4. semantic — the built-in vector index (no pre-filter:
                # the pet scope is enforced by the text leg + the oracle)
                vrank = [r["k"] for r in s.run(
                    "CALL db.index.vector.queryNodes('note_vec', $n, $v) "
                    "YIELD node, score RETURN node.koid AS k "
                    "ORDER BY score DESC", n=500, v=QUERY_VEC)]
                # 5. ranking — app-side RRF over the text + vector legs
                trank = [r["k"] for r in s.run(
                    "MATCH (n:Note) WHERE n.topic = 'pet' "
                    "AND n.body CONTAINS 'cats' "
                    "RETURN n.koid AS k ORDER BY n.seq")]
            ranked = rrf_topk(trank, vrank)
            ok = (anchor == koid and topic == note["topic"]
                  and len(ranked) == 10
                  and all(k in cats_set for k in ranked))
            if topic == "pet":
                ok = ok and reach == kq_expect[koid]
            return ok

        return op

    workloads = run_engine(
        connect, lambda d: d.close(),
        [None, None, None, None, build_graph, None, build_knowledge_query])

    return {"workloads": workloads,
            "rss_kb": docker_rss("bench-neo4j"),
            "disk_bytes": docker_du("bench-neo4j", ["/var/lib/neo4j/data",
                                                    "/data"]),
            "ingest_s": round(ingest_s, 2)}


# ---------------------------------------------------------------- qdrant

from qdrant_client import QdrantClient
from qdrant_client.models import (Distance, PointStruct, VectorParams,
                                   Filter, FieldCondition, MatchValue)

def bench_qdrant(ds):
    q = QdrantClient(host="127.0.0.1", port=6333, timeout=60)
    if q.collection_exists("notes"):
        q.delete_collection("notes")
    q.create_collection("notes",
                        vectors_config=VectorParams(size=2,
                                                    distance=Distance.COSINE))
    t0 = time.perf_counter()
    q.upsert("notes", [PointStruct(id=n["i"], vector=n["vec"],
                                   payload={"topic": n["topic"],
                                            "body": n["body"]})
                       for n in ds["notes"]])
    ingest_s = time.perf_counter() - t0
    q.close()

    cats_ids = {n["i"] for n in ds["notes"] if n["body"] == "cats"}

    def connect():
        return QdrantClient(host="127.0.0.1", port=6333, timeout=60)

    def build_vector(c):
        def op():
            hits = c.query_points("notes", query=QUERY_VEC, limit=10).points
            return (len(hits) == 10
                    and all(h.id in cats_ids for h in hits))

        return op

    workloads = run_engine(
        connect, lambda c: c.close(),
        [None, None, None, None, None, build_vector, None])

    return {"workloads": workloads,
            "rss_kb": docker_rss("bench-qdrant"),
            "disk_bytes": docker_du("bench-qdrant", ["/qdrant/storage"]),
            "ingest_s": round(ingest_s, 2)}


# ---------------------------------------------------------------- mongodb

from pymongo import MongoClient

MONGO_URL = "mongodb://127.0.0.1:27017"


def bench_mongo(ds):
    # CI-07 — the composed "Mongo+vector" stack: Mongo serves the documents
    # and the edges (identity/filter/traversal), a bolted-on vector store
    # (qdrant, the same composition the column's name describes — Mongo's
    # native vector search is Atlas-only, §11: no engine forced into a
    # workload it is not for) serves the semantic leg, and the application
    # fuses the ranking.
    client = MongoClient(MONGO_URL)
    db = client.bench
    db.notes.drop()
    db.events.drop()
    db.mentions.drop()
    db.derived_from.drop()
    note_koids = [f"mongo-note-{n['i']}" for n in ds["notes"]]
    event_koids = [f"mongo-event-{e['i']}" for e in ds["events"]]
    q = QdrantClient(host="127.0.0.1", port=6333, timeout=60)
    if q.collection_exists("mongo_notes"):
        q.delete_collection("mongo_notes")
    q.create_collection("mongo_notes",
                        vectors_config=VectorParams(size=2,
                                                    distance=Distance.COSINE))
    t0 = time.perf_counter()
    db.notes.insert_many(
        [{"_id": k, "seq": n["i"], "topic": n["topic"], "body": n["body"]}
         for k, n in zip(note_koids, ds["notes"])])
    db.events.insert_many(
        [{"_id": k, "seq": e["i"], "label": e["label"]}
         for k, e in zip(event_koids, ds["events"])])
    db.mentions.insert_many(
        [{"note_koid": note_koids[a], "event_koid": event_koids[b]}
         for a, b in ds["mentions"]])
    db.derived_from.insert_many(
        [{"note_koid": note_koids[a], "target_koid": note_koids[b]}
         for a, b in ds["derived"]])
    db.notes.create_index("seq", unique=True)
    db.mentions.create_index("note_koid")
    db.derived_from.create_index("note_koid")
    # The composed stack's vector layer: the Mongo notes mirrored into
    # qdrant (point ids are the note indexes — qdrant accepts only
    # unsigned-int/UUID ids; the Mongo _id is rebuilt from the index) with
    # the topic payload for the scope filter.
    q.upsert("mongo_notes", [PointStruct(id=n["i"], vector=n["vec"],
                                         payload={"topic": n["topic"],
                                                  "body": n["body"]})
                             for n in ds["notes"]])
    ingest_s = time.perf_counter() - t0

    cats_set = {note_koids[n["i"]] for n in ds["notes"] if n["body"] == "cats"}
    read_pairs = [(note_koids[i], ds["notes"][i]) for i in ds["read_ids"]]
    kq_expect = {
        note_koids[i]: ({event_koids[b] for a, b in ds["mentions"] if a == i}
                        | {note_koids[b] for a, b in ds["derived"] if a == i})
        for i in ds["read_ids"]
    }
    pet_filter = Filter(must=[FieldCondition(key="topic",
                                             match=MatchValue(value="pet"))])

    def connect():
        return client

    def build_knowledge_query(c):
        rng = random.Random(7)

        def op():
            koid, note = rng.choice(read_pairs)
            # 1. identity resolution — unique-index lookup
            doc = db.notes.find_one({"seq": note["i"]}, {"_id": 1})
            anchor = doc["_id"] if doc else None
            # 2. metadata filter — the query's scope gate
            tdoc = db.notes.find_one({"_id": anchor}, {"topic": 1}) \
                if anchor else None
            topic = tdoc["topic"] if tdoc else None
            # 3. traversal — app-side edge walk over the edge collections
            reach = set()
            if topic == "pet":
                reach = {m["event_koid"] for m in db.mentions.find(
                    {"note_koid": anchor})}
                reach |= {m["target_koid"] for m in db.derived_from.find(
                    {"note_koid": anchor})}
            # 4. semantic — the bolted-on vector store, pet scope via the
            # payload filter
            hits = q.query_points(
                "mongo_notes", query=QUERY_VEC, limit=500,
                query_filter=pet_filter).points
            vrank = [f"mongo-note-{h.id}" for h in hits]
            # 5. ranking — app-side RRF over the text + vector legs
            trank = [m["_id"] for m in db.notes.find(
                {"topic": "pet", "body": {"$regex": "^cats"}},
                {"_id": 1}).sort("seq", 1)]
            ranked = rrf_topk(trank, vrank)
            ok = (anchor == koid and topic == note["topic"]
                  and len(ranked) == 10
                  and all(k in cats_set for k in ranked))
            if topic == "pet":
                ok = ok and reach == kq_expect[koid]
            return ok

        return op

    workloads = run_engine(
        connect, lambda c: None,
        [None, None, None, None, None, None, build_knowledge_query])

    return {"workloads": workloads,
            "rss_kb": docker_rss("bench-mongo"),
            "disk_bytes": docker_du("bench-mongo", ["/data/db"]),
            "ingest_s": round(ingest_s, 2)}


# ---------------------------------------------------------------- helpers

def docker_rss(name):
    try:
        out = subprocess.run(
            ["docker", "stats", "--no-stream", "--format",
             "{{.Name}} {{.MemUsage}}", name],
            capture_output=True, text=True, check=True).stdout.strip()
        mem = out.split()[1]  # e.g. "123.4MiB"
        units = {"KiB": 1, "MiB": 1024, "GiB": 1024 ** 2, "kB": 1, "MB": 1000}
        for u, mul in units.items():
            if mem.endswith(u):
                return int(float(mem[:-len(u)]) * mul)
        return None
    except Exception as e:
        print(f"  rss {name}: {e}")
        return None


def docker_du(name, paths):
    for p in paths:
        try:
            out = subprocess.run(
                ["docker", "exec", name, "du", "-skL", p],
                capture_output=True, text=True, check=True).stdout
            return int(out.split()[0]) * 1024
        except Exception:
            continue
    return None


def engine_meta():
    import importlib.metadata
    ver = {"aikoql": importlib.metadata.version("aikoql")}
    for name in ["bench-pg", "bench-neo4j", "bench-qdrant", "bench-mongo"]:
        try:
            img = subprocess.run(
                ["docker", "inspect", "--format", "{{.Config.Image}}", name],
                capture_output=True, text=True, check=True).stdout.strip()
            ver[name.removeprefix("bench-")] = img
        except Exception:
            ver[name.removeprefix("bench-")] = None
    return ver


def git_commit():
    return subprocess.run(
        ["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"],
        capture_output=True, text=True).stdout.strip()


def main():
    started = int(time.time() * 1000)
    ds = gen_dataset()
    engines = {}

    # the v2 store adopts no existing directory — hand the SDK a
    # nonexistent path so its fresh-path auto-create initializes it
    kb = Path(tempfile.mkdtemp(prefix="aikoql-bench-")) / "kb"
    print("aikoql ingest + cells ...")
    engines["aikoql"] = bench_aikoql(ds, kb)
    shutil.rmtree(kb, ignore_errors=True)

    print("postgresql ingest + cells ...")
    engines["postgresql"] = bench_pg(ds)

    print("neo4j ingest + cells ...")
    engines["neo4j"] = bench_neo4j(ds)

    print("qdrant ingest + cells ...")
    engines["qdrant"] = bench_qdrant(ds)

    print("mongodb ingest + cells ...")
    engines["mongodb"] = bench_mongo(ds)

    result = {
        "commit": git_commit(),
        "started_at": started,
        "seed": 42,
        "dataset": {"notes": N_NOTES, "events": N_EVENTS,
                    "mentions": len(ds["mentions"]),
                    "derived_from": len(ds["derived"]),
                    "embedding_dim": 2, "n_per_cell": N,
                    "warmup_ops": WARMUP},
        "engine_versions": engine_meta(),
        "engines": engines,
    }
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / "result.json"
    out.write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote {out}")
    for name, e in engines.items():
        wl = ", ".join(f"{w['name']}:{'ok' if w['correct'] else 'FAIL'}"
                       for w in e["workloads"] if w.get("n"))
        print(f"  {name}: ingest {e['ingest_s']}s, rss {e['rss_kb']} KiB, "
              f"disk {e['disk_bytes']} B | {wl}")


if __name__ == "__main__":
    sys.exit(main())
