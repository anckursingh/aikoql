//! P4-M4 — repository complexity audit + temporal/event seek
//! (TDD-KERNEL-001/002, TDD-EVENT-001). ker001 pins the committed
//! classification manifest (exact method set + hot-path classes); ker004
//! pins seek parity against the reference scan; ker002/ker003 are env-gated
//! measurement cells (reported, not asserted — rule 2) whose correctness
//! asserts stay unconditional.

use aikoql_kernel::knowledge::kom::{KnowledgeEvent, KnowledgeObject, Metadata, KOID};
use aikoql_kernel::storage::repository::KnowledgeRepository;
use aikoql_kernel::storage::store::MemoryEngine;
use aikoql_kernel::storage::store_redb::RedbEngine;
use aikoql_kernel::*;
use std::sync::Arc;
use std::time::Instant;

fn tmp_db(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aikoql_seek_{}_{}_{}.redb",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn ko(koid: KOID, version: u64, commit_ts: u64) -> KnowledgeObject {
    let mut k = KnowledgeObject::new(
        koid,
        Metadata {
            type_name: "t".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
        SecurityDescriptor {
            owner: "cell".into(),
            acl: vec![],
            classification: None,
        },
    );
    k.version = version;
    k.commit_ts = commit_ts;
    k
}

fn ke(seq: u64) -> KnowledgeEvent {
    KnowledgeEvent {
        seq,
        koid: KOID::ZERO,
        version: 1,
        kind: EventKind::Updated,
        origin: Origin::System,
        actor: "cell".into(),
        commit_ts: seq,
        payload_hash: [0u8; 32],
        prev_audit_hash: [0u8; 32],
        audit_hash: [0u8; 32],
        signature: None,
        note: None,
    }
}

// ---------------------------------------------------------------------------
// ker001 — the classification manifest exists and pins every public method
// ---------------------------------------------------------------------------

#[test]
fn ker001_classification_manifest_pins_every_public_method() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("REPOSITORY-COMPLEXITY.tsv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("classification manifest missing at {}: {e}", path.display()));
    // method<TAB>class per line — machine-readable, zero parse dependencies.
    let manifest: std::collections::BTreeMap<String, String> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (m, c) = l
                .split_once('\t')
                .expect("manifest line must be method<TAB>class");
            (m.to_string(), c.to_string())
        })
        .collect();

    // The exact public method set — a new public repository method is a
    // compile-time-sized review: it must appear here (and in the committed
    // manifest) with an honest class. This list is the pin.
    let expected: &[&str] = &[
        "new",
        "with_cache",
        "write_batch",
        "write_rel_index",
        "del_rel_index",
        "scan_outbound",
        "scan_inbound",
        "write_type_index",
        "delete_type_index",
        "scan_type",
        "type_index_marker",
        "put_type_index_marker",
        "rebuild_derived_indexes",
        "put_schema_row",
        "schema_rows",
        "current_seq",
        "journal_head",
        "put_journal",
        "get_object_version",
        "get_head",
        "get_heads_many",
        "get_object_versions_many",
        "get_version_at",
        "get_object_at",
        "put_object_version",
        "delete_object_version",
        "put_head",
        "delete_head",
        "scan_heads",
        "scan_object_versions",
        "put_event",
        "get_event",
        "scan_events",
        "scan_events_after",
        "put_idem",
        "get_idem",
        "put_tombstone",
        "get_tombstone",
        "put_subscription",
        "delete_subscription",
        "scan_subscriptions",
    ];
    let mut expected: Vec<&str> = expected.to_vec();
    expected.sort_unstable();
    let got: Vec<&str> = manifest.keys().map(|s| s.as_str()).collect();
    assert_eq!(
        got, expected,
        "manifest method set must match the repository surface"
    );

    // Hot-path pins — the class names are the deliverable's vocabulary.
    let pin: &[(&str, &str)] = &[
        ("get_object_version", "O(1)"),
        ("get_head", "O(1)"),
        ("get_event", "O(1)"),
        ("get_version_at", "O(log N)"),
        ("get_object_at", "O(log N)"),
        ("scan_events_after", "O(log N + matches)"),
        ("scan_type", "O(matches)"),
        ("scan_outbound", "O(matches)"),
        ("scan_inbound", "O(matches)"),
        ("scan_object_versions", "O(versions)"),
        ("scan_heads", "O(N)"),
        ("scan_events", "O(N)"),
    ];
    for (method, class) in pin {
        assert_eq!(
            manifest.get(*method),
            Some(&class.to_string()),
            "{} must be classified {class}",
            method
        );
    }
}

// ---------------------------------------------------------------------------
// ker004 — seek results == reference scan (the old fetch-all path)
// ---------------------------------------------------------------------------

#[test]
fn ker004_seek_parity_object_and_event() {
    let repo = KnowledgeRepository::new(Arc::new(MemoryEngine::new()));
    let koid = KOID::from_bytes([0xAB; 16]);

    // Object leg: versions at ts 1,3,5,7,9 — get_version_at must equal the
    // reference predecessor over a full scan for every snap_ts boundary.
    for (v, ts) in [(1u64, 1u64), (2, 3), (3, 5), (4, 7), (5, 9)] {
        let mut batch = WriteBatch::new();
        repo.put_object_version(&mut batch, &koid, ts, &ko(koid, v, ts));
        repo.write_batch(&batch).unwrap();
    }
    let reference: Vec<(u64, u64)> = repo
        .scan_object_versions(&koid)
        .unwrap()
        .into_iter()
        .map(|(ts, k)| (ts, k.version))
        .collect();
    for snap in 0..=11u64 {
        let expected = reference.iter().rev().find(|(ts, _)| *ts <= snap).copied();
        let got = repo
            .get_version_at(&koid, snap)
            .unwrap()
            .map(|(ts, k)| (ts, k.version));
        assert_eq!(
            got, expected,
            "get_version_at(snap={snap}) must match the reference scan"
        );
    }

    // Event leg: seqs 1..=50 — scan_events_after must equal the reference
    // filter over the full journal for every boundary.
    let mut batch = WriteBatch::new();
    for seq in 1..=50u64 {
        repo.put_event(&mut batch, seq, &ke(seq));
    }
    repo.write_batch(&batch).unwrap();
    let reference: Vec<u64> = repo
        .scan_events()
        .unwrap()
        .into_iter()
        .map(|e| e.seq)
        .collect();
    for after in 0..=52u64 {
        let expected: Vec<u64> = reference.iter().copied().filter(|s| *s > after).collect();
        let got: Vec<u64> = repo
            .scan_events_after(after)
            .unwrap()
            .into_iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(
            got, expected,
            "scan_events_after({after}) must match the reference scan"
        );
    }
}

// ---------------------------------------------------------------------------
// ker002 — version-count sweep cell (env-gated: KER_SWEEP=1)
// ---------------------------------------------------------------------------

#[test]
fn ker002_version_count_sweep_latency_cell() {
    if std::env::var("KER_SWEEP").as_deref() != Ok("1") {
        return; // nightly cell
    }
    let path = tmp_db("sweep");
    let repo = KnowledgeRepository::new(Arc::new(RedbEngine::open(&path).unwrap()));
    let koid = KOID::from_bytes([0xCD; 16]);
    let counts = [10u64, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000];
    for n in counts {
        let mut batch = WriteBatch::new();
        for i in 0..n {
            repo.put_object_version(&mut batch, &koid, i + 1, &ko(koid, i + 1, i + 1));
        }
        repo.write_batch(&batch).unwrap();
        let t = Instant::now();
        let got = repo.get_version_at(&koid, n / 2).unwrap().unwrap();
        let dt = t.elapsed();
        assert_eq!(
            got.1.version,
            n / 2,
            "predecessor must be the ts-n/2 version"
        );
        println!("ker002 versions={n} get_version_at_latency={:?}", dt);
    }
}

// ---------------------------------------------------------------------------
// ker003 — event-replay cell (env-gated: KER_EVENT_CELL=1m|100m)
// ---------------------------------------------------------------------------

#[test]
fn ker003_event_replay_cell() {
    let n: Option<u64> = match std::env::var("KER_EVENT_CELL").as_deref() {
        Ok("1m") => Some(1_000_000),
        Ok("100m") => Some(100_000_000),
        _ => None,
    };
    let Some(n) = n else { return }; // nightly cell
    let path = tmp_db("events");
    let repo = KnowledgeRepository::new(Arc::new(RedbEngine::open(&path).unwrap()));
    let t0 = Instant::now();
    const BATCH: u64 = 10_000;
    for start in (1..=n).step_by(BATCH as usize) {
        let mut batch = WriteBatch::new();
        let end = (start + BATCH - 1).min(n);
        for seq in start..=end {
            repo.put_event(&mut batch, seq, &ke(seq));
        }
        repo.write_batch(&batch).unwrap();
    }
    println!("ker003 events={n} write_elapsed={:?}", t0.elapsed());

    let t = Instant::now();
    let tail = repo.scan_events_after(n - 100).unwrap();
    let dt = t.elapsed();
    assert_eq!(tail.len(), 100, "the last 100 events of the journal");
    assert_eq!(
        tail[0].seq,
        n - 99,
        "seek lands exactly on the first tail event"
    );
    assert_eq!(tail[99].seq, n, "and ends at the head");
    println!("ker003 events={n} tail_100_latency={:?}", dt);
}
