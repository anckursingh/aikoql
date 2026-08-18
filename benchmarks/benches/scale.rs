//! R14 scale scenarios (MVP-readiness benchmark infrastructure).
//!
//! Datasets are built ONCE per run — unlike `knowledge_ops`, which rebuilds
//! per iteration (hence its 50K ceiling). Scale is tunable:
//!
//!   AIKOQL_BENCH_SCALE=10000   cargo bench -p aikoql-benchmarks --bench scale
//!   AIKOQL_BENCH_SCALE=100000  cargo bench -p aikoql-benchmarks --bench scale
//!   AIKOQL_BENCH_SCALE=1000000 ...  # big machines only — the spec's
//!                                   # "1M objects (or until memory limit)"
//!
//! Default is 100_000. Run: `cargo bench -p aikoql-benchmarks --bench scale`
//!
//! Coverage (R14 spec):
//! - writes/sec, reads/sec, query latency p50/p95/p99 (criterion reports
//!   median / p95 / p99 natively per benchmark)
//! - mixed R/W 80/20, concurrent readers (4 threads)
//! - type-indexed scan at scale (kernel-level "prefix query" slot; raw
//!   storage key-prefix scans are covered by storage_scan_benchmark in
//!   aikoql-kernel — R6)
//! - BFS traversal at depth 1/2/3 on an N-edge graph (binary tree, N-1 edges)
//! - 5 canonical aikoql query patterns, planning + execution
//! - dataset metrics: KO count, edge count, on-disk (redb) bytes/KO,
//!   peak RSS (Linux only — /proc/self/status VmHWM)

use aikoql_compiler::{parser, Compiler};
use aikoql_graph::{GraphEngine, RelateRequest, TraverseQuery};
use aikoql_kernel::*;
use aikoql_runtime::Interpreter;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

use std::sync::Arc;
use std::time::Duration;

/// One synthetic doc: type "Doc", body "doc {i}", 128-dim vector (model "bench").
fn remember_doc(kernel: &Kernel, alice: &Subject, i: u64) -> KOID {
    let mut props = PropertyMap::new();
    props.insert("body".into(), Value::Text(format!("doc {i}")));
    let mut emb = Vec::with_capacity(128);
    for j in 0..128 {
        emb.push(((i.wrapping_mul(j + 1)) as f32).sin());
    }
    let semantic = SemanticBlock {
        embedding_model: Some("bench".into()),
        embedding: Some(emb),
        confidence: Some(0.9),
        source: Some("bench".into()),
        summary: None,
    };
    kernel
        .remember(RememberRequest {
            context: alice.into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "Doc".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: Some(semantic),
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid
}

/// Write-payload doc: type "Doc", body text only (no semantic block) —
/// matches the bare write cost measured by knowledge_ops.
fn remember_plain(kernel: &Kernel, alice: &Subject, i: u64) -> KOID {
    let mut props = PropertyMap::new();
    props.insert("body".into(), Value::Text(format!("doc {i}")));
    kernel
        .remember(RememberRequest {
            context: alice.into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "Doc".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid
}

struct Dataset {
    kernel: Kernel,
    koids: Vec<KOID>,
}

fn build_dataset(n: usize) -> Dataset {
    let clock = Arc::new(ManualClock::new(20_000));
    let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0x5CA1E).unwrap();
    let alice = Subject::new("bench");

    let mut koids = Vec::with_capacity(n);
    for i in 0..n as u64 {
        koids.push(remember_doc(&kernel, &alice, i));
    }

    // Binary-tree edges: node i -> children 2i+1, 2i+2. N-1 edges.
    // ponytail: relate-after is O(n) full-KO rewrites (~1 min at 100K);
    // a batch edge-insertion API would be the upgrade path.
    for i in 0..n {
        for child in [2 * i + 1, 2 * i + 2] {
            if child >= n {
                break;
            }
            GraphEngine::relate(
                &kernel,
                RelateRequest::new(&alice, koids[i], koids[child], "knows"),
            )
            .unwrap();
        }
    }

    Dataset { kernel, koids }
}

/// ponytail: default 100K; 1M runs need ~4 GB with MemoryEngine — the
/// AIKOQL_BENCH_SCALE knob is the spec's "or until memory limit".
fn scale() -> usize {
    std::env::var("AIKOQL_BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000)
}

/// One-shot dataset metrics: KO/edge counts, redb on-disk bytes at 1K KOs,
/// peak RSS (Linux). Printed once per run — not benchmarked.
fn report_metrics(ds: &Dataset) {
    let n = ds.koids.len();

    // On-disk size via a throwaway redb store (MemoryEngine has no disk).
    let redb_path = std::env::temp_dir().join(format!("aikoql-bench-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&redb_path);
    let mut disk_bytes_per_ko = 0u64;
    {
        let clock = Arc::new(ManualClock::new(20_000));
        let kernel = Kernel::open(
            Arc::new(RedbEngine::open(&redb_path).unwrap()),
            clock,
            0x5CA1E,
        )
        .unwrap();
        let alice = Subject::new("bench");
        for i in 0..1_000u64 {
            remember_doc(&kernel, &alice, i);
        }
        drop(kernel);
        if let Ok(md) = std::fs::metadata(&redb_path) {
            disk_bytes_per_ko = md.len() / 1_000;
        }
        let _ = std::fs::remove_file(&redb_path);
    }

    #[cfg(target_os = "linux")]
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(|l| l.trim_start_matches("VmHWM:").trim().to_string())
        })
        .unwrap_or_else(|| "unavailable".into());
    #[cfg(not(target_os = "linux"))]
    let rss = "n/a (non-Linux)";

    eprintln!(
        "aikoql-bench scale={n} edges={} redb_disk_bytes_per_ko={} peak_rss_kb={}",
        n - 1,
        disk_bytes_per_ko,
        rss
    );
}

fn bench_scale(c: &mut Criterion) {
    let n = scale();
    let ds = build_dataset(n);
    report_metrics(&ds);

    let mut group = c.benchmark_group("scale");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));

    let alice = Subject::new("bench");
    let mut counter = 0u64;
    let mut w = 0u64;

    // Reads: get() by KOID — criterion reports p50/p95/p99 natively.
    group.throughput(Throughput::Elements(1));
    group.bench_function(format!("read/get_n={n}"), |b| {
        b.iter(|| {
            counter += 1;
            black_box(
                ds.kernel
                    .get(alice.clone(), &ds.koids[counter as usize % n])
                    .unwrap(),
            )
        })
    });

    // Type-indexed scan (kernel-level "prefix query" slot).
    group.throughput(Throughput::Elements(n as u64));
    group.bench_function(format!("read/scan_type_n={n}"), |b| {
        b.iter(|| black_box(ds.kernel.scan_by_type(&alice, "Doc").unwrap()))
    });

    // Writes: fresh remember per iteration against a small scratch store —
    // the read-heavy scenarios must not see a growing dataset. The scratch
    // store itself grows over the measurement window (~350K KOs / 700 MB
    // peak at full speed) and is dropped after this benchmark.
    let scratch = build_dataset(1_000);
    group.throughput(Throughput::Elements(1));
    group.bench_function(format!("write/remember_n={n}"), |b| {
        b.iter(|| {
            w += 1;
            black_box(remember_plain(&scratch.kernel, &alice, w))
        })
    });

    // BFS traversal from the tree root at depth 1/2/3 on the N-1-edge graph.
    for depth in [1usize, 2, 3] {
        let q = TraverseQuery::new(alice.clone(), ds.koids[0]).with_depth(depth);
        group.bench_function(format!("traverse/depth_{depth}_edges_{}", n - 1), |b| {
            b.iter(|| black_box(GraphEngine::traverse(&ds.kernel, q.clone()).unwrap()))
        });
    }

    // 5 canonical aikoql patterns — planning + execution per iteration.
    let traverse_json = format!(r#"{{"traverse":{{"start":"{}","depth":3}}}}"#, ds.koids[0]);
    let patterns: [(&str, &str, bool); 5] = [
        ("scan", "MATCH Doc RETURN *", false),
        (
            "filter",
            r#"MATCH Doc WHERE body == "doc 42" RETURN *"#,
            false,
        ),
        ("text", r#"MATCH Doc SIMILAR TO "doc 42" RETURN *"#, false),
        (
            "fuse",
            r#"MATCH Doc SIMILAR TO "doc 42" SCORE BM25 USING EMBEDDING RETURN *"#,
            false,
        ),
        ("traverse_json", traverse_json.as_str(), true),
    ];
    for (name, source, is_json) in &patterns {
        group.bench_function(format!("aikoql/{name}_plan_exec"), |b| {
            b.iter(|| {
                let plan = if *is_json {
                    Compiler::compile(source).unwrap()
                } else {
                    parser::compile_with_subject(source, "bench").unwrap()
                };
                black_box(Interpreter::execute(&ds.kernel, &plan).unwrap())
            })
        });
    }

    // Mixed R/W: 80% reads on the big dataset, 20% writes on a scratch
    // store (keeps the read dataset static). The scratch grows over the
    // window and is dropped after this benchmark.
    let scratch_rw = build_dataset(1_000);
    group.throughput(Throughput::Elements(10));
    group.bench_function(format!("mixed_rw_80_20_n={n}"), |b| {
        b.iter(|| {
            let mut sum = 0u64;
            for _ in 0..8 {
                counter += 1;
                sum += ds
                    .kernel
                    .get(alice.clone(), &ds.koids[counter as usize % n])
                    .unwrap()
                    .version;
            }
            for _ in 0..2 {
                w += 1;
                remember_plain(&scratch_rw.kernel, &alice, w);
            }
            black_box(sum)
        })
    });

    // Concurrent readers: 4 threads x 25 gets.
    group.throughput(Throughput::Elements(100));
    group.bench_function(format!("concurrent_reads_4t_n={n}"), |b| {
        b.iter(|| {
            let hits = std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(4);
                for t in 0..4u64 {
                    let kernel = &ds.kernel;
                    let koids = &ds.koids;
                    handles.push(s.spawn(move || {
                        let alice = Subject::new("bench");
                        let mut sum = 0u64;
                        for i in 0..25u64 {
                            sum += kernel
                                .get(alice.clone(), &koids[((t * 25 + i) as usize) % n])
                                .unwrap()
                                .version;
                        }
                        sum
                    }));
                }
                handles.into_iter().map(|h| h.join().unwrap()).sum::<u64>()
            });
            black_box(hits)
        })
    });

    group.finish();
}

criterion_group!(benches, bench_scale);
criterion_main!(benches);
