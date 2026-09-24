//! P5-M4 (ND-04) — streaming/batch execution. st001–st010.
//!
//! The roadmap's ND-04 RED list verbatim: empty input, single row, batch
//! boundaries, huge result, cancellation, authorization, snapshots,
//! backpressure, memory bounds. The streaming surface is
//! `aikoql_runtime::streaming::{execute_streaming, PhysicalOperator, …}` — a
//! pull-based pipeline (next_batch), so backpressure is by construction: a
//! slow consumer stops pulling and no operator holds more than one batch plus
//! its upstream source. The supported streaming shape is
//! Scan → Filter → Project → Limit; anything else fails closed.
//!
//! Honest ledger (st004/st007): the RSS cell (gate 7) lands with the W-suite
//! sampler harness — the batch bound here is the structural half of the pin.
//! st010 (P5-M21): the open pin is version-level — the koid list AND a
//! snapshot timestamp are captured at open, so every batch serves one
//! consistent version set (st007 asserts the row set, st010 the payloads).

use aikoql_compiler::parser;
use aikoql_kernel::security::crypto::{Aes256Gcm, Crypto, CryptoProvider};
use aikoql_kernel::security::envelope::Envelope;
use aikoql_kernel::security::field_crypto::EncryptionPolicy;
use aikoql_kernel::security::kms::KeyManager;
use aikoql_kernel::transaction::kernel::{KnowledgeContext, Subject};
use aikoql_kernel::*;
use aikoql_runtime::streaming::{
    execute_streaming, CancellationToken, IndexStrategy, PhysicalOperator, ScanOperator,
    StreamOptions,
};
use aikoql_runtime::Interpreter;
use std::sync::{Arc, RwLock};

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn ctx(name: &str) -> KnowledgeContext {
    KnowledgeContext::new(Subject::new(name))
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

/// Remember a Person named `name`; returns its KOID.
fn person(k: &Kernel, name: &str) -> KOID {
    let mut req = RememberRequest::create(ctx("alice"), meta("Person"));
    req.properties
        .insert("name".into(), Value::Text(name.into()));
    k.remember(req).unwrap().koid
}

/// Pull every batch of a streaming pipeline; returns (row koids, batch sizes).
fn collect(pipe: &mut dyn PhysicalOperator) -> (Vec<KOID>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut batches = Vec::new();
    while let Some(batch) = pipe.next_batch().unwrap() {
        batches.push(batch.len());
        rows.extend(batch.into_iter().map(|ko| ko.koid));
    }
    (rows, batches)
}

/// Stream `query` on `k` with the given batch size; returns (row koids, batch sizes).
/// Streams as alice — the owner of every row created by `person` (the default
/// authorization is owner-only, so an arbitrary subject would see nothing).
fn stream(k: &Kernel, query: &str, batch_size: usize) -> (Vec<KOID>, Vec<usize>) {
    let plan = parser::compile_physical_with_subject(query, "alice").unwrap();
    let opts = StreamOptions {
        batch_size,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(k, &plan, &opts).unwrap();
    collect(&mut pipe)
}

/// Materialized execution of the same query — the parity oracle.
fn materialized(k: &Kernel, query: &str, subject: &str) -> Vec<KOID> {
    let plan = parser::compile_with_subject(query, subject).unwrap();
    match Interpreter::execute(k, &plan).unwrap() {
        aikoql_runtime::RowSet::Objects(kos) => kos.into_iter().map(|ko| ko.koid).collect(),
        other => panic!("expected Objects, got {}", other.shape()),
    }
}

// --- st001 — empty input -------------------------------------------------------

#[test]
fn st001_empty_input_streams_zero_batches() {
    let k = mk();
    let plan = parser::compile_physical("MATCH Person RETURN *").unwrap();
    let opts = StreamOptions {
        batch_size: 4,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    assert!(
        pipe.next_batch().unwrap().is_none(),
        "empty type → no batches"
    );
}

// --- st002 — single row ----------------------------------------------------------

#[test]
fn st002_single_row_arrives_in_one_batch() {
    let k = mk();
    person(&k, "Alice");
    let plan = parser::compile_physical_with_subject("MATCH Person RETURN *", "alice").unwrap();
    let opts = StreamOptions {
        batch_size: 16,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let first = pipe.next_batch().unwrap().expect("one batch");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].properties["name"], Value::Text("Alice".into()));
    assert!(pipe.next_batch().unwrap().is_none(), "then exhausted");
}

// --- st003 — batch boundaries -----------------------------------------------------

#[test]
fn st003_batch_boundaries_match_materialized_row_for_row() {
    let k = mk();
    for i in 0..7 {
        person(&k, &format!("P{i}"));
    }
    let query = "MATCH Person RETURN *";
    let (stream_ids, batches) = stream(&k, query, 3);
    assert_eq!(batches, vec![3, 3, 1], "batch boundaries at batch_size");
    let mat_ids = materialized(&k, query, "alice");
    assert_eq!(stream_ids, mat_ids, "same order, same rows");
}

// --- st004 — huge result (env-gated RSS cell, gate 7) ----------------------------

#[test]
fn st004_huge_result_memory_bounded_env_gated() {
    if std::env::var("P5M4_HUGE").is_err() {
        eprintln!("skipped: set P5M4_HUGE=1 to run the huge-result cell");
        return;
    }
    let k = mk();
    let n = 50_000;
    for i in 0..n {
        person(&k, &format!("P{i}"));
    }
    let (ids, batches) = stream(&k, "MATCH Person RETURN *", 100);
    assert_eq!(ids.len(), n, "every row streams");
    assert!(
        batches.iter().all(|b| *b <= 100),
        "no batch exceeds batch_size"
    );
    // Gate-7 RSS cell (peak RSS independent of result cardinality) lands with
    // the W-suite sampler harness; the batch bound is the structural half.
}

// --- st005 — cancellation ---------------------------------------------------------

#[test]
fn st005_cancellation_stops_mid_scan() {
    let k = mk();
    for i in 0..20 {
        person(&k, &format!("P{i}"));
    }
    let cancel = CancellationToken::new();
    let mut scan = ScanOperator::new(&k, Subject::new("alice"), "Person", 4, cancel.clone());
    scan.open().unwrap();
    let first = scan.next_batch().unwrap().expect("first batch");
    assert_eq!(first.len(), 4);
    cancel.cancel();
    assert!(
        matches!(scan.next_batch().unwrap_err(), KError::Cancelled),
        "token dropped mid-scan → Cancelled, no partial write"
    );
    scan.close().unwrap();
}

// --- st006 — authorization ---------------------------------------------------------

#[test]
fn st006_authorization_excludes_role_scoped_rows_identically() {
    let k = mk();
    // 3 rows bob may read (explicit Allow) + 2 locked to alice only. The
    // default authorization is owner-only, so every row needs its ACL.
    let bob_visible = SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![
            AclEntry {
                principal: "bob".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
            AclEntry {
                principal: "alice".into(),
                action: Action::Read,
                effect: Effect::Allow,
            },
        ],
        classification: None,
    };
    let alice_only = SecurityDescriptor {
        owner: "alice".into(),
        acl: vec![AclEntry {
            principal: "alice".into(),
            action: Action::Read,
            effect: Effect::Allow,
        }],
        classification: None,
    };
    for i in 0..3 {
        let mut req = RememberRequest::create(ctx("alice"), meta("Person"));
        req.properties
            .insert("name".into(), Value::Text(format!("open{i}")));
        req.security = Some(bob_visible.clone());
        k.remember(req).unwrap();
    }
    for i in 0..2 {
        let mut req = RememberRequest::create(ctx("alice"), meta("Person"));
        req.properties
            .insert("name".into(), Value::Text(format!("locked{i}")));
        req.security = Some(alice_only.clone());
        k.remember(req).unwrap();
    }

    let query = "MATCH Person RETURN *";
    // Streaming as bob and the kernel's own materialized scan must agree —
    // the ACL filtering is the kernel's, the stream only feeds it batches.
    let (stream_ids, _) = {
        let plan = parser::compile_physical_with_subject(query, "bob").unwrap();
        let opts = StreamOptions {
            batch_size: 2,
            cancel: CancellationToken::new(),
            index_strategy: IndexStrategy::Scan,
        };
        collect(&mut execute_streaming(&k, &plan, &opts).unwrap())
    };
    let expected = k
        .scan_by_type(&Subject::new("bob"), "Person")
        .unwrap()
        .into_iter()
        .map(|ko| ko.koid)
        .collect::<Vec<_>>();
    assert_eq!(
        expected.len(),
        3,
        "test precondition: bob reads the 3 open rows, not the 2 locked ones"
    );
    assert_eq!(stream_ids, expected, "identical ACL exclusion");
}

// --- st007 — snapshot at open, zero divergence -------------------------------------

#[test]
fn st007_snapshot_pinned_at_open_zero_divergence() {
    let k = mk();
    let ids: Vec<KOID> = (0..4).map(|i| person(&k, &format!("P{i}"))).collect();
    // The expected rowset at open time — captured BEFORE any mid-stream write.
    let expected_open = materialized(&k, "MATCH Person RETURN *", "alice");
    assert_eq!(expected_open.len(), 4);

    let plan = parser::compile_physical_with_subject("MATCH Person RETURN *", "alice").unwrap();
    let opts = StreamOptions {
        batch_size: 2,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let _first = pipe.next_batch().unwrap().unwrap(); // rows 0-1

    // Mid-stream: update a not-yet-emitted row, insert a new one, tombstone one.
    let mut upd = RememberRequest::update(ctx("alice"), ids[3], meta("Person"));
    upd.properties
        .insert("name".into(), Value::Text("P3-new".into()));
    k.remember(upd).unwrap();
    let new_id = person(&k, "P-new");
    k.forget(
        Subject::new("alice"),
        &ids[2],
        ForgetMode::Tombstone,
        None,
        None,
    )
    .unwrap();

    // The koid list AND the version set were captured at open: the insert
    // never appears, and rows alive at open still stream — the tombstone
    // and the update happened after open, so they do not apply (their open
    // versions stream instead; st010 pins the payload-level detail).
    let (rest, _) = collect(&mut pipe);
    assert!(!rest.contains(&new_id), "insert after open never appears");
    assert_eq!(
        rest,
        vec![ids[2], ids[3]],
        "rows alive at open stream, post-open writes never apply"
    );
}

// --- st010 — snapshot at open, VERSION-level (PR6 P0-08) --------------------------

/// The open snapshot is a version contract, not just a koid-list contract:
/// a write between open() and a later batch must not leak into that batch —
/// the tombstoned row still streamed (it existed at open), the updated row
/// streams as its OPEN payload, and the insert never appears. Today the
/// payloads resolve live, so the updated row streams as its new value and
/// the tombstoned row is skipped — mixed-time reads.
#[test]
fn st010_all_batches_see_the_open_snapshot_payloads() {
    let k = mk();
    let ids: Vec<KOID> = (0..4).map(|i| person(&k, &format!("P{i}"))).collect();

    let plan = parser::compile_physical_with_subject("MATCH Person RETURN *", "alice").unwrap();
    let opts = StreamOptions {
        batch_size: 2,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let _first = pipe.next_batch().unwrap().unwrap(); // rows 0-1, emitted pre-write

    // Writes between open() and the next batch: update a not-yet-emitted row,
    // tombstone another, insert a new one.
    let mut upd = RememberRequest::update(ctx("alice"), ids[3], meta("Person"));
    upd.properties
        .insert("name".into(), Value::Text("P3-new".into()));
    k.remember(upd).unwrap();
    k.forget(
        Subject::new("alice"),
        &ids[2],
        ForgetMode::Tombstone,
        None,
        None,
    )
    .unwrap();
    let new_id = person(&k, "P-new");

    let mut rest = Vec::new();
    while let Some(batch) = pipe.next_batch().unwrap() {
        rest.extend(batch);
    }

    assert_eq!(
        rest.iter().map(|ko| ko.koid).collect::<Vec<_>>(),
        vec![ids[2], ids[3]],
        "the tombstoned row existed at open — it streams; the insert does not"
    );
    let name = |ko: &KnowledgeObject| -> String {
        match ko.properties.get("name") {
            Some(Value::Text(t)) => t.clone(),
            _ => panic!("expected a name property"),
        }
    };
    assert_eq!(
        name(&rest[0]),
        "P2",
        "the tombstone after open never applies"
    );
    assert_eq!(name(&rest[1]), "P3", "the update after open never applies");
    let _ = new_id;
}

// --- st008 — backpressure (pull model) ---------------------------------------------

#[test]
fn st008_pull_model_backpressure_batches_never_exceed_bound() {
    let k = mk();
    for i in 0..10 {
        person(&k, &format!("P{i}"));
    }
    let (ids, batches) = stream(&k, "MATCH Person RETURN *", 1);
    assert_eq!(ids.len(), 10);
    assert_eq!(batches, vec![1; 10], "the consumer controls the pace");
    // Nothing is buffered ahead of demand: after exhaustion, still None.
    let plan = parser::compile_physical_with_subject("MATCH Person RETURN *", "alice").unwrap();
    let opts = StreamOptions {
        batch_size: 1,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    while pipe.next_batch().unwrap().is_some() {}
    assert!(pipe.next_batch().unwrap().is_none(), "no buffered tail");
}

// --- st009 — encrypted-DB parity ----------------------------------------------------

/// In-memory KMS for tests (mirrors the kernel encryption-suite helper).
struct MemKms {
    key: RwLock<[u8; 32]>,
}
impl MemKms {
    fn new() -> Self {
        MemKms {
            key: RwLock::new(Aes256Gcm::new().generate_key()),
        }
    }
}
impl KeyManager for MemKms {
    fn master_key(&self, _passphrase: &str) -> Result<[u8; 32], String> {
        Ok(*self.key.read().unwrap())
    }
    fn rotate(&self, _passphrase: &str, provider: &dyn CryptoProvider) -> Result<[u8; 32], String> {
        let new_key = provider.generate_key();
        *self.key.write().unwrap() = new_key;
        Ok(new_key)
    }
}

#[test]
fn st009_encrypted_db_streams_identical_rows() {
    let plain = mk();
    let names: Vec<String> = (0..5).map(|i| format!("P{i}")).collect();
    for n in &names {
        person(&plain, n);
    }

    let kms = MemKms::new();
    let crypto = Arc::new(Crypto::new(Box::new(Aes256Gcm::new())));
    let envelope = Arc::new(Envelope::init(&kms, "pw", crypto.clone()).unwrap());
    let enc = Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xBEEF,
    )
    .unwrap()
    .with_field_encryption(crypto, envelope)
    .unwrap();
    enc.set_encryption_policy("Person", EncryptionPolicy::new(vec!["name".to_string()]));
    for n in &names {
        person(&enc, n);
    }

    // KOIDs embed the kernel instance id, so compare the streamed NAMES.
    let names_of = |k: &Kernel| -> Vec<String> {
        let mut out: Vec<String> = stream(k, "MATCH Person RETURN *", 2)
            .0
            .into_iter()
            .map(
                |koid| match &k.get(Subject::new("alice"), &koid).unwrap().properties["name"] {
                    Value::Text(t) => t.clone(),
                    other => panic!("expected Text name, got {:?}", other),
                },
            )
            .collect();
        out.sort();
        out
    };
    let a = names_of(&plain);
    let b = names_of(&enc);
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(a, expected, "every row streams");
    assert_eq!(
        a, b,
        "decrypted reads flow through the transparent path — identical rows"
    );
}

// --- idx2-008 (P5-M8) — property-index scans on the M4 read path ---------------

#[test]
fn idx2_008_index_assisted_scan_pins_the_index_snapshot_at_open() {
    let k = mk();
    k.catalog_create_index("by_name", "Person", &["name"])
        .unwrap();
    let a = person(&k, "Alice");
    let b = person(&k, "Bob");
    // a matching row the index never saw — under EventualIndex it stays
    // invisible (the explicit eventual choice), while the plain scan still
    // answers the committed truth
    let unindexed = person(&k, "Alice");
    for id in [a, b] {
        let ko = k.get(Subject::new("alice"), &id).unwrap();
        for idx in k.property_indexes().unwrap() {
            idx.upsert(id, &ko).unwrap();
        }
    }

    let plan = parser::compile_physical_with_subject(
        "MATCH Person WHERE name == \"Alice\" RETURN *",
        "alice",
    )
    .unwrap();

    let opts = StreamOptions {
        batch_size: 4,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::EventualIndex,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let (rows, _) = collect(&mut pipe);
    assert_eq!(
        rows,
        vec![a],
        "the index scan answers exactly the indexed koids"
    );

    // control: the plain Scan answers the open snapshot (committed truth)
    let opts = StreamOptions {
        batch_size: 4,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::Scan,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let (rows, _) = collect(&mut pipe);
    assert!(
        rows.contains(&a) && rows.contains(&unindexed),
        "the plain scan answers the committed truth"
    );

    // snapshot: a row created AFTER open never appears in either mode
    // (the koid list is pinned at open — the ScanOperator contract)
    let opts = StreamOptions {
        batch_size: 4,
        cancel: CancellationToken::new(),
        index_strategy: IndexStrategy::EventualIndex,
    };
    let mut pipe = execute_streaming(&k, &plan, &opts).unwrap();
    let late = person(&k, "Alice");
    assert!(
        !collect(&mut pipe).0.contains(&late),
        "rows created after open never appear"
    );
}
