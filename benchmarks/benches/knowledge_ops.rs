//! Benchmark: core knowledge operations at scale.
//!
//! Measures write throughput, read latency, and scan performance at
//! increasing dataset sizes (1K, 10K, 50K objects).
//!
//! Run with: `cargo bench -p aikoql-benchmarks`
//!
//! ponytail: 50K max (not 1M). 1M takes minutes per iteration —
//! Criterion's default sampling is impractical at that scale.
//! Add 100K/1M as a separate nightly bench when needed.
//! ponytail: traversal skipped — needs graph engine dep. Add when
//! benchmarks crate depends on aikoql-graph.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use aikoql_kernel::*;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared — deterministic spread for read tests
// ---------------------------------------------------------------------------

fn spread_koid(koids: &[KOID], iter: u64) -> KOID {
    koids[(iter as usize * 7 + 13) % koids.len()]
}

// ---------------------------------------------------------------------------
// Write throughput — create N fresh KOs in a batch
// ---------------------------------------------------------------------------

fn bench_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_throughput");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(15));

    for n in [1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_function(format!("remember_n={}", n), |b| {
            b.iter_batched(
                || {
                    let clock = Arc::new(ManualClock::new(20_000));
                    Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap()
                },
                |kernel| {
                    let subject = Subject::new("bench");
                    for i in 0..n {
                        let mut props = PropertyMap::new();
                        props.insert("v".into(), Value::Int(i as i64));
                        black_box(
                            kernel
                                .remember(RememberRequest {
                                    context: (&subject).into(),
                                    koid: None,
                                    expected_version: Some(0),
                                    idempotency_key: None,
                                    metadata: Metadata {
                                        type_name: "bench".into(),
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
                                .unwrap(),
                        );
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Read latency — get() by KOID, cold-cache worst case
// ---------------------------------------------------------------------------

fn bench_read_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_latency");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    for n in [1_000u64, 10_000] {
        // Build dataset once, then benchmark reads against it.
        let clock = Arc::new(ManualClock::new(20_000));
        let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap();
        let subject = Subject::new("bench");
        let mut koids = Vec::with_capacity(n as usize);
        for i in 0..n {
            let mut props = PropertyMap::new();
            props.insert("body".into(), Value::Text(format!("doc {}", i)));
            let r = kernel
                .remember(RememberRequest {
                    context: (&subject).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: None,
                    metadata: Metadata {
                        type_name: "doc".into(),
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
                .unwrap();
            koids.push(r.koid);
        }

        let mut counter: u64 = 0;
        group.bench_function(format!("get_n={}", n), |b| {
            b.iter(|| {
                counter += 1;
                black_box(
                    kernel
                        .get(subject.clone(), &spread_koid(&koids, counter))
                        .unwrap(),
                )
            })
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Scan — scan_by_type, the most common read pattern
// ---------------------------------------------------------------------------

fn bench_scan_type(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan_type");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(15));

    for n in [1_000u64, 10_000] {
        let clock = Arc::new(ManualClock::new(20_000));
        let kernel = Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap();
        let subject = Subject::new("bench");
        for i in 0..n {
            let mut props = PropertyMap::new();
            props.insert("body".into(), Value::Text(format!("doc {}", i)));
            kernel
                .remember(RememberRequest {
                    context: (&subject).into(),
                    koid: None,
                    expected_version: Some(0),
                    idempotency_key: None,
                    metadata: Metadata {
                        type_name: "doc".into(),
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
                .unwrap();
        }

        group.throughput(Throughput::Elements(n));
        group.bench_function(format!("scan_n={}", n), |b| {
            b.iter(|| {
                black_box(kernel.scan_by_type(&subject, "doc").unwrap())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_write_throughput, bench_read_latency, bench_scan_type);
criterion_main!(benches);
