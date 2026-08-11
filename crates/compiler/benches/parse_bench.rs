//! Parser benchmark — MRFC-0010 §11 gate: 100 KB query < 20 ms.
//!
//! Generates a realistic query with many WHERE conditions then measures
//! end-to-end `parser::compile()` latency.

use aikoql_compiler::parser;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Build a query string of approximately `size_kb` kilobytes.
fn build_fat_query(size_kb: usize) -> String {
    let target_bytes = size_kb * 1024;
    let cond = r#"company == "Visa" OR city == "Amsterdam" OR role == "engineer""#;
    let mut q = String::with_capacity(target_bytes + 64);
    q.push_str("MATCH Person WHERE ");
    q.push_str(cond);
    while q.len() < target_bytes - 16 {
        q.push_str(" AND ");
        q.push_str(cond);
    }
    q.push_str(" RETURN *");
    q
}

fn bench_parse_1kb(c: &mut Criterion) {
    let q = build_fat_query(1);
    c.bench_function("parse_1kb", |b| b.iter(|| parser::compile(black_box(&q))));
}

fn bench_parse_10kb(c: &mut Criterion) {
    let q = build_fat_query(10);
    c.bench_function("parse_10kb", |b| b.iter(|| parser::compile(black_box(&q))));
}

fn bench_parse_100kb(c: &mut Criterion) {
    let q = build_fat_query(100);
    c.bench_function("parse_100kb", |b| b.iter(|| parser::compile(black_box(&q))));
}

fn bench_parse_simple(c: &mut Criterion) {
    let q = "MATCH Person WHERE company == \"Visa\" RETURN *";
    c.bench_function("parse_simple", |b| b.iter(|| parser::compile(black_box(q))));
}

fn bench_parse_hybrid(c: &mut Criterion) {
    let q = "MATCH Person SIMILAR TO \"John\" TRAVERSE managed_by WHERE company == \"Visa\" RETURN explain";
    c.bench_function("parse_hybrid", |b| b.iter(|| parser::compile(black_box(q))));
}

criterion_group!(
    benches,
    bench_parse_simple,
    bench_parse_hybrid,
    bench_parse_1kb,
    bench_parse_10kb,
    bench_parse_100kb
);
criterion_main!(benches);
