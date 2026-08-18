//! Throughput comparison: sequential vs parallel directory ingestion (R10.2).
//! Builds synthetic rust-file trees and measures extraction wall-time for
//! each mode. Run: cargo bench -p aikoql-ingestion --bench ingest_benchmark

use aikoql_ingestion::{ingest_directory, parallel_ingest_directory};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fs;

/// Synthetic tree: `n` small rust files spread over 20 subdirs.
fn build_tree(n_files: usize) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!("aikoql-ingest-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    for i in 0..n_files {
        let dir = tmp.join(format!("mod{}", i % 20));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("f{i}.rs")),
            format!(
                "//! file {i}\npub fn f{i}() {{}}\nstruct S{i};\nimpl S{i} {{ pub fn m(&self) {{}} }}\n"
            ),
        )
        .unwrap();
    }
    tmp
}

fn ingest_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("ingest");
    group.sample_size(10);
    for n in [100usize, 500] {
        let tmp = build_tree(n);
        let root = tmp.to_string_lossy().to_string();
        group.bench_function(format!("sequential_{n}_files"), |b| {
            b.iter(|| ingest_directory(black_box(&root)).unwrap())
        });
        group.bench_function(format!("parallel_{n}_files"), |b| {
            b.iter(|| parallel_ingest_directory(black_box(&root)).unwrap())
        });
        let _ = fs::remove_dir_all(&tmp);
    }
    group.finish();
}

criterion_group!(benches, ingest_benchmark);
criterion_main!(benches);
