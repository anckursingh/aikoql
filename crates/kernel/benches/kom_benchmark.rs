use aikoql_kernel::codec::{decode_ko, encode_ko};
use aikoql_kernel::kom::{
    AclEntry, Action, Direction, Effect, EventKind, EventRef, IdGen, KnowledgeObject, Lifecycle,
    LifecycleState, Metadata, Origin, RelationshipRef, SecurityDescriptor, SemanticBlock, Value,
    KOID,
};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::collections::BTreeMap;

fn sample_ko() -> KnowledgeObject {
    let mut properties = BTreeMap::new();
    properties.insert("name".into(), Value::Text("aikoql".into()));
    properties.insert("score".into(), Value::Float(0.99));
    properties.insert("rank".into(), Value::Int(-7));
    properties.insert(
        "nested".into(),
        Value::Map(BTreeMap::from([(
            "list".into(),
            Value::List(vec![
                Value::Bool(true),
                Value::Null,
                Value::Bytes(vec![1, 2, 3]),
            ]),
        )])),
    );
    let mut extensions = BTreeMap::new();
    extensions.insert("x-future-field".into(), Value::Text("preserve me".into()));
    KnowledgeObject {
        koid: IdGen::new(9).next(1234),
        version: 3,
        commit_ts: 0xdead_beef,
        metadata: Metadata {
            type_name: "fact".into(),
            tenant: Some("acme".into()),
            schema_version: 2,
            tags: vec!["ai".into(), "memory".into()],
        },
        properties,
        semantic: Some(SemanticBlock {
            embedding_model: Some("bge-m3".into()),
            embedding: Some(vec![0.1, -2.5, 3.75]),
            confidence: Some(0.98),
            source: Some("sec-filing".into()),
            summary: Some("Revenue grew 10%".into()),
        }),
        relationships: vec![
            RelationshipRef {
                rel_type: "cites".into(),
                target: KOID([7u8; 16]),
                direction: Direction::Outbound,
            },
            RelationshipRef {
                rel_type: "derived-from".into(),
                target: KOID([8u8; 16]),
                direction: Direction::Inbound,
            },
        ],
        event_refs: vec![EventRef {
            seq: 42,
            kind: EventKind::Updated,
            commit_ts: 0xdead_beef,
        }],
        security: SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![
                AclEntry {
                    principal: "bob".into(),
                    action: Action::Read,
                    effect: Effect::Allow,
                },
                AclEntry {
                    principal: "contractors".into(),
                    action: Action::Write,
                    effect: Effect::Deny,
                },
            ],
            classification: Some("internal".into()),
        },
        lifecycle: Lifecycle {
            state: LifecycleState::Verified,
            origin: Origin::Agent("agent-007".into()),
        },
        extensions,
    }
}

fn koid_benchmark(c: &mut Criterion) {
    let mut gen = IdGen::new(42);
    c.bench_function("koid_generation", |b| {
        b.iter(|| black_box(gen.next(black_box(1_000_000))))
    });
}

fn lifecycle_benchmark(c: &mut Criterion) {
    c.bench_function("lifecycle_can_transition", |b| {
        b.iter(|| {
            black_box(LifecycleState::Draft.can_transition(black_box(LifecycleState::Active)))
        })
    });
}

fn codec_benchmark(c: &mut Criterion) {
    let ko = sample_ko();
    let bytes = encode_ko(&ko);
    c.bench_function("ko_encode", |b| b.iter(|| encode_ko(black_box(&ko))));
    c.bench_function("ko_decode", |b| {
        b.iter(|| decode_ko(black_box(&bytes)).unwrap())
    });
    c.bench_function("ko_roundtrip", |b| {
        b.iter(|| {
            let bytes = encode_ko(black_box(&ko));
            decode_ko(&bytes).unwrap()
        })
    });
}

fn validation_benchmark(c: &mut Criterion) {
    let ko = sample_ko();
    c.bench_function("ko_validate", |b| {
        b.iter(|| black_box(ko.validate()).unwrap())
    });
}

criterion_group!(
    benches,
    koid_benchmark,
    lifecycle_benchmark,
    codec_benchmark,
    validation_benchmark
);
criterion_main!(benches);
