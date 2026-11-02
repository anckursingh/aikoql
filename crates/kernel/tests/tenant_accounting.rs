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
