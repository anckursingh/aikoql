//! AG-32 — tenant accounting from KO metadata: the create/forget/transact
//! paths keep the TenantManager's usage counters in sync with the object
//! store, and `rebuild` resolves each head's tenant from its metadata.
//! Gate: usage per tenant matches the oracle count.

use aikoql_kernel::*;
use std::sync::Arc;

fn mk_kernel() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    (k, clock)
}

fn remember(k: &Kernel, tenant: Option<&str>) -> KOID {
    let mut ctx = KnowledgeContext::new(Subject::new("alice"));
    ctx.tenant = tenant.map(String::from);
    let mut req = RememberRequest::create(
        ctx,
        Metadata {
            type_name: "Memory".into(),
            tenant: tenant.map(String::from),
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("content".into(), Value::Text("scratch".into()));
    k.remember(req).unwrap().koid
}

#[test]
fn ag32_usage_matches_oracle_after_creates() {
    let (k, _clock) = mk_kernel();
    remember(&k, Some("a"));
    remember(&k, Some("a"));
    remember(&k, Some("b"));
    remember(&k, None);
    assert_eq!(k.tenant_usage("a"), 2);
    assert_eq!(k.tenant_usage("b"), 1);
    assert_eq!(k.tenant_usage("default"), 1);
    assert_eq!(k.tenant_usage("other"), 0);
}

#[test]
fn ag32_forget_decrements_its_tenant() {
    let (k, _clock) = mk_kernel();
    let first = remember(&k, Some("a"));
    remember(&k, Some("a"));
    assert_eq!(k.tenant_usage("a"), 2);
    k.forget(
        Subject::new("alice"),
        &first,
        ForgetMode::Tombstone,
        None,
        None,
    )
    .unwrap();
    assert_eq!(k.tenant_usage("a"), 1);
}

#[test]
fn ag32_reopen_rebuilds_usage_from_store() {
    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    remember(&k, Some("a"));
    remember(&k, Some("a"));
    remember(&k, None);
    drop(k);
    // a fresh kernel over the same store must rebuild usage from the
    // heads instead of starting at zero and re-learning as writes land
    let k2 = Kernel::open(store.clone(), clock, 0xE915).unwrap();
    assert_eq!(k2.tenant_usage("a"), 2);
    assert_eq!(k2.tenant_usage("default"), 1);
}

#[test]
fn ag32_transact_creates_are_counted() {
    let (k, _clock) = mk_kernel();
    let req = |tenant: Option<&str>| {
        let mut ctx = KnowledgeContext::new(Subject::new("alice"));
        ctx.tenant = tenant.map(String::from);
        RememberRequest::create(
            ctx,
            Metadata {
                type_name: "Memory".into(),
                tenant: tenant.map(String::from),
                schema_version: 1,
                tags: vec![],
            },
        )
    };
    let ops = vec![
        TransactionOp::new(KnowledgeContext::new(Subject::new("alice")), req(Some("x"))),
        TransactionOp::new(KnowledgeContext::new(Subject::new("alice")), req(Some("x"))),
        TransactionOp::new(KnowledgeContext::new(Subject::new("alice")), req(None)),
    ];
    k.transact(ops).unwrap();
    assert_eq!(k.tenant_usage("x"), 2);
    assert_eq!(k.tenant_usage("default"), 1);
}

/// AG-32b: usage persists under a head watermark so a clean reopen loads
/// the rows instead of decoding every head; a database without the
/// watermark (an older binary) still rebuilds from the heads.
#[test]
fn ag32_usage_persists_under_a_watermark_and_rebuilds_when_missing() {
    // Wire-format pins — the kernel's rows (u64 LE counts under a
    // per-tenant key, a (seq, ts) head watermark).
    const HEAD: &[u8] = b"__tenants/usage-head";
    const PREFIX: &[u8] = b"__tenants/usage/";

    let clock = Arc::new(ManualClock::new(10_000));
    let store = Arc::new(MemoryEngine::new());
    let k = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    remember(&k, Some("a"));
    remember(&k, Some("a"));
    remember(&k, None);
    drop(k);

    // every commit rode the watermark, and the create commits rode their
    // tenant's usage row
    assert!(store.get(HEAD).unwrap().is_some(), "watermark persisted");
    assert_eq!(
        store.get(b"__tenants/usage/a").unwrap().map(|v| v.len()),
        Some(8),
        "tenant a's row persisted"
    );
    // a, default — plus the catalog bootstrap's own tenant
    let rows = store.scan(PREFIX).unwrap();
    assert_eq!(rows.len(), 3, "one row per tenant, nothing more");
    assert_eq!(
        store
            .get(b"__tenants/usage/aikoql:catalog")
            .unwrap()
            .map(|v| v.len()),
        Some(8),
        "the catalog object counts on its own tenant"
    );

    // a clean reopen loads the rows — counts identical to a rebuild
    let k2 = Kernel::open(store.clone(), clock.clone(), 0xE915).unwrap();
    assert_eq!(k2.tenant_usage("a"), 2);
    assert_eq!(k2.tenant_usage("default"), 1);
    drop(k2);

    // simulate a pre-watermark database: drop the watermark and the rows,
    // the reopen must rebuild both the counts and the persisted rows
    let mut b = WriteBatch::new();
    b.del(HEAD.to_vec());
    b.del(b"__tenants/usage/a".to_vec());
    b.del(b"__tenants/usage/default".to_vec());
    store.write_batch(&b).unwrap();
    let k3 = Kernel::open(store.clone(), clock, 0xE915).unwrap();
    assert_eq!(k3.tenant_usage("a"), 2);
    assert_eq!(k3.tenant_usage("default"), 1);
    drop(k3);
    assert_eq!(
        store.scan(PREFIX).unwrap().len(),
        3,
        "rows re-persisted (catalog included)"
    );
    assert!(store.get(HEAD).unwrap().is_some(), "watermark re-persisted");
}
