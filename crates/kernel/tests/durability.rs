//! Durability & crash-recovery acceptance suite (Phase 1, Increment 2).
//!
//! Gates (VISION-AND-STRATEGY §3 Phase 1):
//! - committed mutations survive process restart AND abrupt termination
//!   (kill -9 / power-loss at the commit boundary);
//! - journal sequence + audit chain continue unbroken across reopen;
//! - write batches are all-or-nothing on disk;
//! - P99 point-read latency below the NFR threshold on the bench dataset.

use aikoql_kernel::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn tmp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aikoql_dur_{}_{}_{}.redb",
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

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn kernel_at(path: &PathBuf, salt: u64) -> Kernel {
    let engine = RedbEngine::open(path).expect("open engine");
    Kernel::open(Arc::new(engine), Arc::new(SystemClock), salt).expect("open kernel")
}

// ---------------------------------------------------------------------------
// Restart durability
// ---------------------------------------------------------------------------

#[test]
fn d01_committed_mutations_survive_restart() {
    let path = tmp_db("restart");
    let id;
    {
        let k = kernel_at(&path, 1);
        let r = k
            .remember(RememberRequest::create(alice(), meta("fact")))
            .unwrap();
        id = r.koid;
        let mut up = RememberRequest::update(alice(), id, meta("fact"));
        up.properties.insert("n".into(), Value::Int(42));
        k.remember(up).unwrap();
    } // kernel + engine dropped (clean close)

    let k2 = kernel_at(&path, 1);
    let ko = k2.get(alice(), &id).unwrap();
    assert_eq!(ko.version, 2);
    assert_eq!(ko.properties.get("n"), Some(&Value::Int(42)));
    assert_eq!(k2.journal().unwrap().len(), 2);
    assert!(k2.prove(alice(), &id).unwrap().chain_valid);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn d02_journal_seq_and_hlc_continue_after_reopen() {
    let path = tmp_db("continuity");
    let id;
    let head0;
    {
        let k = kernel_at(&path, 2);
        id = k
            .remember(RememberRequest::create(alice(), meta("fact")))
            .unwrap()
            .koid;
        head0 = k.journal_head().unwrap();
    }
    let k2 = kernel_at(&path, 2);
    assert_eq!(k2.journal_head().unwrap(), head0);
    let r = k2
        .remember(RememberRequest::update(alice(), id, meta("fact")))
        .unwrap();
    // HLC was re-seeded: the new commit_ts strictly exceeds the pre-restart one
    assert!(r.commit_ts > 0);
    let j = k2.journal().unwrap();
    assert_eq!(j.len(), 2);
    assert_eq!(j[1].seq, 2);
    assert!(
        j[1].commit_ts > j[0].commit_ts,
        "commit_ts must be monotone across restarts"
    );
    assert!(k2.prove(alice(), &id).unwrap().chain_valid);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn d03_batch_is_all_or_nothing_on_disk() {
    let path = tmp_db("atomic");
    let engine = RedbEngine::open(&path).unwrap();
    let mut b = WriteBatch::new();
    for i in 0..100u8 {
        b.put(vec![i], vec![i.wrapping_mul(3)]);
    }
    engine.write_batch(&b).unwrap();
    assert_eq!(engine.scan(&[]).unwrap().len(), 100);

    let mut b2 = WriteBatch::new();
    for i in 0..50u8 {
        b2.del(vec![i]);
    }
    b2.put(b"marker".to_vec(), vec![1]);
    engine.write_batch(&b2).unwrap();
    assert_eq!(engine.scan(&[]).unwrap().len(), 51);
    assert_eq!(engine.get(b"marker").unwrap(), Some(vec![1]));
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Crash recovery (abrupt termination — no destructors)
// ---------------------------------------------------------------------------

#[test]
fn d04_abrupt_termination_preserves_all_commits() {
    let path = tmp_db("crash");
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../target/debug/examples/crash_writer");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "crash_writer example not built at {:?}; run `cargo build --examples` first",
        exe
    );
    let out = std::process::Command::new(&exe)
        .arg(&path)
        .arg("7")
        .output()
        .expect("spawn crash_writer");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("COMMITTED_SEQ=7"),
        "unexpected writer output: {} (stderr: {})",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );

    // reopen after the "crash" and verify every commit + the audit chain
    let k = kernel_at(&path, 7);
    let (seq, _) = k.journal_head().unwrap();
    assert_eq!(seq, 7, "journal head must survive abrupt termination");
    let crasher = Subject::new("crasher");
    for i in 0..7u8 {
        let id = KOID::from_bytes([i; KOID_LEN]);
        let ko = k
            .get(&crasher, &id)
            .unwrap_or_else(|e| panic!("missing KO {}: {}", i, e));
        assert_eq!(ko.properties.get("i"), Some(&Value::Int(i as i64)));
    }
    let proof = k
        .prove(&crasher, &KOID::from_bytes([0u8; KOID_LEN]))
        .unwrap();
    assert!(proof.chain_valid, "audit chain must validate after crash");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn d04b_crash_fuzz_random_commit_boundaries() {
    let mut exe = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    exe.push("../../target/debug/examples/crash_writer");
    #[cfg(windows)]
    exe.set_extension("exe");
    assert!(
        exe.exists(),
        "crash_writer example not built at {:?}; run `cargo build --examples` first",
        exe
    );

    // Deterministic fuzz: crash after every possible prefix boundary.
    let n = 15u8;
    for crash_after in 0..=n {
        let path = tmp_db("crash_fuzz");
        let out = std::process::Command::new(&exe)
            .arg(&path)
            .arg(n.to_string())
            .arg(crash_after.to_string())
            .output()
            .expect("spawn crash_writer");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let expected = format!("COMMITTED_SEQ={}", crash_after);
        assert!(
            stdout.contains(&expected),
            "crash_after={}: unexpected output: {} (stderr: {})",
            crash_after,
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );

        let k = kernel_at(&path, 7);
        let (seq, _) = k.journal_head().unwrap();
        assert_eq!(
            seq, crash_after as u64,
            "crash_after={}: committed prefix must survive",
            crash_after
        );
        let crasher = Subject::new("crasher");
        for i in 0..crash_after {
            let id = KOID::from_bytes([i; KOID_LEN]);
            let ko = k
                .get(&crasher, &id)
                .unwrap_or_else(|e| panic!("missing KO {}: {}", i, e));
            assert_eq!(ko.properties.get("i"), Some(&Value::Int(i as i64)));
        }
        if crash_after > 0 {
            let proof = k
                .prove(&crasher, &KOID::from_bytes([0u8; KOID_LEN]))
                .unwrap();
            assert!(
                proof.chain_valid,
                "chain must validate after crash at {}",
                crash_after
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// Engine interop + on-disk conformance smoke
// ---------------------------------------------------------------------------

#[test]
fn d05_syscall_surface_behaves_identically_on_durable_engine() {
    let path = tmp_db("interop");
    let k = kernel_at(&path, 3);
    // same flow as the reference conformance suite, engine-agnostic
    let a = k
        .remember(RememberRequest::create(alice(), meta("fact")))
        .unwrap()
        .koid;
    k.evolve(
        alice(),
        &a,
        LifecycleState::Active,
        Origin::Human,
        None,
        None,
    )
    .unwrap();
    let mut up = RememberRequest::update(alice(), a, meta("fact"));
    up.properties
        .insert("body".into(), Value::Text("durable cats".into()));
    up.semantic = Some(SemanticBlock {
        embedding_model: Some("m".into()),
        embedding: Some(vec![1.0, 0.0]),
        confidence: None,
        source: None,
        summary: None,
    });
    k.remember(up).unwrap();

    let res = k
        .find_similar(SimilarityQuery {
            context: alice().into(),
            filter: None,
            text: Some("cats".into()),
            vector: Some(vec![1.0, 0.0]),
            embedding_model: None,
            k: 3,
            fusion: Fusion::Rrf { k0: 60 },
        })
        .unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].ko.koid, a);

    let lin = k.trace(alice(), &a).unwrap();
    assert_eq!(lin.versions.len(), 3);
    k.forget(alice(), &a, ForgetMode::Tombstone, None, None)
        .unwrap();
    assert!(k.prove(alice(), &a).unwrap().chain_valid);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn d06_concurrent_writers_gapless_journal_on_disk() {
    let path = tmp_db("conc");
    let k = Arc::new(kernel_at(&path, 4));
    let mut handles = Vec::new();
    for t in 0..2 {
        let k = k.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                let s = Subject::new(&format!("w{}-{}", t, i));
                k.remember(RememberRequest::create(s, meta("fact")))
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let j = k.journal().unwrap();
    assert_eq!(j.len(), 50);
    for (i, ke) in j.iter().enumerate() {
        assert_eq!(ke.seq, (i + 1) as u64);
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// P99 latency gate (NFR: point read < 10 ms) — run explicitly:
//   cargo test -p aikoql-kernel --test durability -- --ignored d07
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn d07_point_read_p99_gate() {
    let path = tmp_db("bench");
    let k = kernel_at(&path, 9);
    let mut ids = Vec::new();
    for i in 0..500 {
        let mut req = RememberRequest::create(alice(), meta("fact"));
        req.properties.insert("i".into(), Value::Int(i));
        ids.push(k.remember(req).unwrap().koid);
    }
    let mut lat = Vec::with_capacity(ids.len());
    for id in &ids {
        let t = Instant::now();
        k.get(alice(), id).unwrap();
        lat.push(t.elapsed());
    }
    lat.sort();
    let p50 = lat[lat.len() / 2];
    let p99 = lat[(lat.len() * 99) / 100];
    println!(
        "BENCH point-read n={} p50={:?} p99={:?} (engine=redb, dataset=500 KOs)",
        lat.len(),
        p50,
        p99
    );
    assert!(
        p99 < Duration::from_millis(10),
        "P99 gate breached: {:?}",
        p99
    );
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// REC-002 backup/restore knowledge-equivalence (MVP-QA-001)
// ---------------------------------------------------------------------------

#[test]
fn d09_restore_preserves_relations_provenance_temporal_and_constraints() {
    // MVP-QA-001 REC-002: backup → destroy → restore yields equivalent KOs,
    // facts, relations, provenance, temporal state AND constraints.
    // Red 2026-08-25: the schema registry is in-memory only — after restore
    // the check-violating write was accepted (constraints lost).
    let path = tmp_db("rec002");
    let snap = tmp_db("rec002snap");
    let salt = 0xC0FFEE;
    let (catalog, item, asserted);
    {
        let k = kernel_at(&path, salt);
        k.register_schema(
            Schema::new("Item", 1)
                .required_property("name", "Text")
                .property("qty", "Int")
                .check(
                    "qty_positive",
                    CheckExpression::Compare {
                        op: CompareOp::Gt,
                        left: Box::new(CheckExpression::Property("qty".into())),
                        right: Box::new(CheckExpression::Literal(Value::Int(0))),
                    },
                ),
        )
        .unwrap();

        // relations: Catalog <- Item (Outbound from Item)
        catalog = k
            .remember(RememberRequest::create(alice(), meta("Catalog")))
            .unwrap()
            .koid;
        let mut item_req = RememberRequest::create(alice(), meta("Item"));
        item_req
            .properties
            .insert("name".into(), Value::Text("widget".into()));
        item_req.properties.insert("qty".into(), Value::Int(7));
        item_req.relationships.push(RelationshipRef {
            rel_type: "listed_in".into(),
            target: catalog,
            direction: Direction::Outbound,
        });
        item = k.remember(item_req).unwrap().koid;

        // provenance + temporal state: an asserted KO carrying evidence and
        // an explicit assertion instant.
        asserted = k
            .assert_knowledge(AssertionRequest {
                context: alice().into(),
                type_name: "Policy".into(),
                properties: {
                    let mut p = PropertyMap::new();
                    p.insert("text".into(), Value::Text("retention is 30 days".into()));
                    p
                },
                authority: Some("architecture_decision".into()),
                evidence: vec![Evidence::new("runbook.md", EvidenceMethod::DocExtraction)],
                valid_from: Some(1_000),
                security: None,
                note: None,
            })
            .unwrap()
            .koid;

        k.backup_store_to(&snap).unwrap();
    }

    // destroy → fresh kernel → restore → reopen (restart after restore)
    std::fs::remove_file(&path).unwrap();
    {
        let k = kernel_at(&path, salt);
        k.restore_store_from(&snap).unwrap();
    }
    let k = kernel_at(&path, salt);

    // relations equivalent
    let restored_item = k.get(alice(), &item).unwrap();
    assert_eq!(restored_item.relationships.len(), 1);
    assert_eq!(restored_item.relationships[0].target, catalog);

    // provenance + temporal state equivalent
    let restored_asserted = k.get(alice(), &asserted).unwrap();
    match restored_asserted
        .extensions
        .get(KnowledgeObject::EXT_EVIDENCE)
    {
        Some(Value::List(items)) => assert!(!items.is_empty(), "evidence survives restore"),
        other => panic!("expected evidence list after restore, got {other:?}"),
    }
    assert_eq!(
        restored_asserted
            .extensions
            .get(KnowledgeObject::EXT_VALID_FROM),
        Some(&Value::Int(1_000)),
        "assertion instant survives restore"
    );

    // constraints equivalent — the red leg: a qty<=0 write must be REJECTED
    let mut bad = RememberRequest::create(alice(), meta("Item"));
    bad.properties
        .insert("name".into(), Value::Text("bad".into()));
    bad.properties.insert("qty".into(), Value::Int(-3));
    assert!(
        k.remember(bad).is_err(),
        "restored kernel must enforce the registered check constraint"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&snap);
}
