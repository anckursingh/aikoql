//! AG-13 — memory consolidation: near-duplicate KOs are merged by a
//! background job so the surface stays small without losing provenance.
//! The merge derives a new KO with `derived_from` edges to every source
//! (AG-13a: lineage kept) and deletes nothing — sources stay readable and
//! the journal records no Forgotten (AG-13c: only `forget` deletes).
//!
//! RED: `aikoql_scheduler::consolidate` and `ConsolidationJob` do not exist.

use aikoql_kernel::transaction::kernel::{KnowledgeContext, ManualClock, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_scheduler::{consolidate, decay, ConsolidationJob};
use std::sync::Arc;
use std::time::Duration;

/// Property-similarity gate for near-duplicates (0.8 = conservative).
const THRESHOLD: f64 = 0.8;

/// The session-memory TTL the decay leg uses (tests tick a ManualClock).
const SESSION_TTL_MS: u64 = 60_000;

/// Salience below this demotes a TTL-expired session memory.
const DEMOTE_BELOW: f64 = 0.5;

/// The job's subject: an unscoped admin — the only principal the kernel
/// lets read across tenants (auth T-55).
fn subject() -> Subject {
    Subject::with_roles("consolidation", &["admin"])
}

fn open(tag: &str) -> Kernel {
    let dir = std::env::temp_dir().join(format!("ag13-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine =
        Arc::new(aikoql_storage_v2::AikoqlStorageEngineV2::open(dir.to_str().unwrap()).unwrap());
    Kernel::open(engine, Arc::new(SystemClock), 0xA113).unwrap()
}

fn note(k: &Kernel, body: &str, tag: &str) -> KOID {
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("ag13")),
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.properties.insert("tag".into(), Value::Text(tag.into()));
    k.remember(req).unwrap().koid
}

/// A scope-tagged Memory KO — the shape the mcp remember tool stores.
/// The origin picks the authority: Human → human_approved (rank 10),
/// Agent → agent_derived (rank 2) — the kernel stamps the extension.
fn memory(k: &Kernel, scope: &str, origin: Origin) -> KOID {
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("ag13")),
        Metadata {
            type_name: "Memory".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.origin = origin;
    req.properties
        .insert("scope".into(), Value::Text(scope.into()));
    req.properties
        .insert("content".into(), Value::Text("session scratch".into()));
    k.remember(req).unwrap().koid
}

fn open_clocked(tag: &str) -> (Kernel, Arc<ManualClock>) {
    let dir = std::env::temp_dir().join(format!("ag13-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let engine =
        Arc::new(aikoql_storage_v2::AikoqlStorageEngineV2::open(dir.to_str().unwrap()).unwrap());
    let clock = Arc::new(ManualClock::new(1_000_000));
    (Kernel::open(engine, clock.clone(), 0xA113).unwrap(), clock)
}

#[test]
fn ag13a_lineage_kept() {
    let k = open("lineage");
    let a = note(&k, "sally works at acme corp as staff engineer", "who");
    let b = note(&k, "sally works at acme corp as a staff engineer", "who");
    let c = note(&k, "the office moves to the harbor in june", "where");
    let merged = consolidate(&k, &subject(), THRESHOLD).unwrap();
    assert_eq!(
        merged.len(),
        1,
        "one near-duplicate pair, one merge: {merged:?}"
    );
    // The merged KO carries the sources as inbound derived_from edges
    // (the index direction is source → derived).
    let inn = k
        .inbound_edges(&merged[0], Some(DERIVED_FROM))
        .unwrap_or_else(|e| panic!("no derived_from edges on the merged KO: {e}"));
    let mut sources: Vec<String> = inn.into_iter().map(|(_, id)| id.to_hex()).collect();
    sources.sort();
    assert_eq!(
        sources,
        vec![a.to_hex(), b.to_hex()],
        "the merged KO traces to both sources"
    );
    assert!(
        !sources.contains(&c.to_hex()),
        "the distinct note must not be a source"
    );
    // NewestWins: the merged properties carry the newest body.
    let merged_ko = k.get(KnowledgeContext::new(subject()), &merged[0]).unwrap();
    assert_eq!(
        merged_ko.properties.get("body"),
        Some(&Value::Text(
            "sally works at acme corp as a staff engineer".into()
        ))
    );
}

#[test]
fn ag13c_nothing_deleted() {
    let k = open("undelete");
    let a = note(&k, "sally works at acme corp as staff engineer", "who");
    let b = note(&k, "sally works at acme corp as a staff engineer", "who");
    let c = note(&k, "the office moves to the harbor in june", "where");
    let merged = consolidate(&k, &subject(), THRESHOLD).unwrap();
    assert_eq!(merged.len(), 1);
    let ctx = || KnowledgeContext::new(subject());
    for id in [a, b, c] {
        k.get(ctx(), &id)
            .unwrap_or_else(|e| panic!("{id} deleted by consolidation: {e}"));
    }
    let forgotten = k
        .journal()
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Forgotten))
        .count();
    assert_eq!(forgotten, 0, "only `forget` deletes");
    assert_eq!(
        k.scan_by_type(&subject(), "note").unwrap().len(),
        4,
        "3 sources + 1 merged — nothing removed"
    );
    // A second run must not merge the same pair again: the sources are
    // already derived_from sources (no duplicate merges, no growth).
    assert_eq!(
        consolidate(&k, &subject(), THRESHOLD).unwrap().len(),
        0,
        "consolidation is idempotent"
    );
}

#[test]
fn ag13_job_consolidates_on_cadence() {
    let k = open("job");
    let job = ConsolidationJob::start(&k, Duration::from_millis(50)).unwrap();
    note(&k, "sally works at acme corp as staff engineer", "who");
    note(&k, "sally works at acme corp as a staff engineer", "who");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while k.scan_by_type(&subject(), "note").unwrap().len() != 3
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    job.shutdown();
    assert_eq!(
        k.scan_by_type(&subject(), "note").unwrap().len(),
        3,
        "the job merged the near-duplicates on its cadence"
    );
}

#[test]
fn ag13_salience_stamped_at_remember() {
    let k = open("salience");
    // salience = recency × access × links × authority; the first three are
    // 1 at birth, so the stamp is the normalized authority rank.
    let koid = memory(&k, "session", Origin::Human); // rank 10
    let ko = k.get(KnowledgeContext::new(subject()), &koid).unwrap();
    assert_eq!(
        ko.extensions.get(KnowledgeObject::EXT_SALIENCE),
        Some(&Value::Float(1.0)),
        "remember stamps salience from the authority rank"
    );
    let agent = memory(&k, "session", Origin::Agent("ag13".into())); // rank 2
    let ko = k.get(KnowledgeContext::new(subject()), &agent).unwrap();
    assert_eq!(
        ko.extensions.get(KnowledgeObject::EXT_SALIENCE),
        Some(&Value::Float(0.2))
    );
    // The key is kernel-managed: a caller cannot mint it.
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("ag13")),
        Metadata {
            type_name: "Memory".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.extensions.insert("salience".into(), Value::Float(5.0));
    assert!(
        k.remember(req).is_err(),
        "caller-supplied salience is rejected"
    );
}

#[test]
fn ag13_access_refresh_bumps_salience() {
    let k = open("access");
    let koid = memory(&k, "shared", Origin::Agent("ag13".into())); // authority 0.2
    k.record_access(std::slice::from_ref(&koid));
    let r = k
        .refresh_salience(KnowledgeContext::new(subject()), &koid, 1, SESSION_TTL_MS)
        .unwrap()
        .unwrap();
    // recency ~1.0 × (1 + 1 access) × authority 0.2 × links 1 ≈ 0.4 (a few
    // real-clock ms of age shave the recency a hair)
    assert!((r - 0.4).abs() < 0.01, "salience {r} ≈ 0.4");
    let ko = k.get(KnowledgeContext::new(subject()), &koid).unwrap();
    assert_eq!(
        ko.extensions.get(KnowledgeObject::EXT_SALIENCE),
        Some(&Value::Float(r))
    );
    let version = ko.version;
    // A refresh with the same count writes nothing (no version churn).
    let r2 = k
        .refresh_salience(KnowledgeContext::new(subject()), &koid, 1, SESSION_TTL_MS)
        .unwrap();
    assert_eq!(r2, None);
    assert_eq!(
        k.get(KnowledgeContext::new(subject()), &koid)
            .unwrap()
            .version,
        version
    );
}

#[test]
fn ag13d_decay_demotes_expired_low_salience_session_memories() {
    let (k, clock) = open_clocked("decay");
    let low = memory(&k, "session", Origin::Agent("ag13".into())); // authority 0.2
    let high = memory(&k, "session", Origin::Human); // authority 1.0
    let shared = memory(&k, "shared", Origin::Agent("ag13".into())); // per-scope: shared has no TTL
    clock.tick(SESSION_TTL_MS);
    let fresh = memory(&k, "session", Origin::Agent("ag13".into())); // young
    clock.tick(SESSION_TTL_MS / 2); // low/high/shared at 1.5×TTL, fresh at TTL/2
                                    // Access keeps hot memories alive — high was recalled once.
    k.record_access(std::slice::from_ref(&high));
    let demoted = decay(&k, &subject(), SESSION_TTL_MS, DEMOTE_BELOW).unwrap();
    assert_eq!(demoted, 1, "only the expired low-salience memory demotes");
    let status = |koid: &KOID| {
        k.get(KnowledgeContext::new(subject()), koid)
            .unwrap()
            .epistemic_status()
    };
    assert_eq!(status(&low), EpistemicStatus::Superseded);
    assert_ne!(
        status(&high),
        EpistemicStatus::Superseded,
        "high authority survives the TTL"
    );
    assert_ne!(
        status(&shared),
        EpistemicStatus::Superseded,
        "shared scope has no TTL"
    );
    assert_ne!(
        status(&fresh),
        EpistemicStatus::Superseded,
        "fresh session memory survives"
    );
    // Demotion is not deletion: every memory stays readable, no Forgotten.
    for id in [low, high, shared, fresh] {
        k.get(KnowledgeContext::new(subject()), &id).unwrap();
    }
    let forgotten = k
        .journal()
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Forgotten))
        .count();
    assert_eq!(forgotten, 0, "only `forget` deletes");
}
