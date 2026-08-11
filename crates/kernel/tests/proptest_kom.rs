//! Property-based conformance tests for the Knowledge Object Model (MRFC-0001 §14).
//!
//! These tests exercise the deterministic binary codec with randomized values
//! to catch round-trip and canonicalization bugs that example-based tests miss.

use aikoql_kernel::codec::{decode_ke, decode_ko, encode_ke, encode_ko};
use aikoql_kernel::kom::{
    AclEntry, Action, Direction, Effect, EventKind, EventRef, IdGen, KnowledgeEvent,
    KnowledgeObject, Lifecycle, LifecycleState, Metadata, Origin, RelationshipRef,
    SecurityDescriptor, SemanticBlock, Value, KOID, KOID_LEN,
};
use proptest::collection;
use proptest::prelude::*;

fn koid_strategy() -> impl Strategy<Value = KOID> {
    any::<[u8; KOID_LEN]>().prop_map(KOID)
}

fn value_strategy() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<f64>().prop_map(Value::Float),
        "[a-zA-Z0-9 _-]{0,32}".prop_map(Value::Text),
        collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
    ];
    leaf.prop_recursive(
        4,  // max depth
        64, // max total size
        8,  // items per collection
        |inner| {
            prop_oneof![
                collection::vec(inner.clone(), 0..8).prop_map(Value::List),
                collection::btree_map("[a-zA-Z0-9]{0,10}", inner.clone(), 0..8)
                    .prop_map(Value::Map),
            ]
        },
    )
}

fn metadata_strategy() -> impl Strategy<Value = Metadata> {
    (
        "[a-zA-Z0-9:_-]{1,32}",
        proptest::option::of("[a-zA-Z0-9]{0,16}"),
        any::<u32>(),
        collection::vec("[a-zA-Z0-9]{1,16}", 0..8),
    )
        .prop_map(|(type_name, tenant, schema_version, tags)| Metadata {
            type_name,
            tenant,
            schema_version,
            tags,
        })
}

fn semantic_block_strategy() -> impl Strategy<Value = SemanticBlock> {
    (
        proptest::option::of("[a-zA-Z0-9:-]{0,32}"),
        proptest::option::of(collection::vec(any::<f32>(), 1..16)),
        proptest::option::of(any::<f32>()),
        proptest::option::of("[a-zA-Z0-9]{0,32}"),
        proptest::option::of("[a-zA-Z0-9 .]{0,64}"),
    )
        .prop_map(
            |(embedding_model, embedding, confidence, source, summary)| SemanticBlock {
                embedding_model,
                embedding,
                confidence,
                source,
                summary,
            },
        )
}

fn relationship_ref_strategy() -> impl Strategy<Value = RelationshipRef> {
    ("[a-zA-Z0-9:_-]{1,32}", koid_strategy(), any::<bool>()).prop_map(
        |(rel_type, target, outbound)| RelationshipRef {
            rel_type,
            target,
            direction: if outbound {
                Direction::Outbound
            } else {
                Direction::Inbound
            },
        },
    )
}

fn event_ref_strategy() -> impl Strategy<Value = EventRef> {
    (any::<u64>(), any::<u8>(), any::<u64>()).prop_map(|(seq, kind_tag, commit_ts)| EventRef {
        seq,
        kind: EventKind::from_tag(kind_tag % 6).unwrap_or(EventKind::Created),
        commit_ts,
    })
}

fn action_strategy() -> impl Strategy<Value = Action> {
    any::<u8>().prop_map(|t| Action::from_tag(t % 5).unwrap_or(Action::Read))
}

fn effect_strategy() -> impl Strategy<Value = Effect> {
    any::<u8>().prop_map(|t| Effect::from_tag(t % 2).unwrap_or(Effect::Allow))
}

fn acl_entry_strategy() -> impl Strategy<Value = AclEntry> {
    ("[a-zA-Z0-9:_-]{1,32}", action_strategy(), effect_strategy()).prop_map(
        |(principal, action, effect)| AclEntry {
            principal,
            action,
            effect,
        },
    )
}

fn security_descriptor_strategy() -> impl Strategy<Value = SecurityDescriptor> {
    (
        "[a-zA-Z0-9:_-]{1,32}",
        collection::vec(acl_entry_strategy(), 0..8),
        proptest::option::of("[a-zA-Z0-9]{0,16}"),
    )
        .prop_map(|(owner, acl, classification)| SecurityDescriptor {
            owner,
            acl,
            classification,
        })
}

fn lifecycle_state_strategy() -> impl Strategy<Value = LifecycleState> {
    any::<u8>().prop_map(|t| LifecycleState::from_tag(t % 5).unwrap_or(LifecycleState::Draft))
}

fn origin_strategy() -> impl Strategy<Value = Origin> {
    prop_oneof![
        Just(Origin::Human),
        "[a-zA-Z0-9:_-]{1,32}".prop_map(Origin::Agent),
        Just(Origin::SemanticEnrichment),
        Just(Origin::Reason),
        Just(Origin::System),
    ]
}

fn lifecycle_strategy() -> impl Strategy<Value = Lifecycle> {
    (lifecycle_state_strategy(), origin_strategy())
        .prop_map(|(state, origin)| Lifecycle { state, origin })
}

fn knowledge_object_strategy() -> impl Strategy<Value = KnowledgeObject> {
    (
        koid_strategy(),
        any::<u64>(),
        any::<u64>(),
        metadata_strategy(),
        collection::btree_map("[a-zA-Z0-9]{1,16}", value_strategy(), 0..16),
        proptest::option::of(semantic_block_strategy()),
        collection::vec(relationship_ref_strategy(), 0..8),
        collection::vec(event_ref_strategy(), 0..8),
        security_descriptor_strategy(),
        lifecycle_strategy(),
        collection::btree_map("[a-zA-Z0-9]{1,16}", value_strategy(), 0..8),
    )
        .prop_map(
            |(
                koid,
                version,
                commit_ts,
                metadata,
                properties,
                semantic,
                relationships,
                event_refs,
                security,
                lifecycle,
                extensions,
            )| {
                KnowledgeObject {
                    koid,
                    version,
                    commit_ts,
                    metadata,
                    properties,
                    semantic,
                    relationships,
                    event_refs,
                    security,
                    lifecycle,
                    extensions,
                }
            },
        )
}

fn hash256_strategy() -> impl Strategy<Value = [u8; 32]> {
    proptest::collection::vec(any::<u8>(), 32).prop_map(|v| {
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        a
    })
}

fn knowledge_event_strategy() -> impl Strategy<Value = KnowledgeEvent> {
    (
        any::<u64>(),
        koid_strategy(),
        any::<u64>(),
        any::<u8>(),
        origin_strategy(),
        "[a-zA-Z0-9:_-]{1,32}",
        any::<u64>(),
        hash256_strategy(),
        hash256_strategy(),
        hash256_strategy(),
        proptest::option::of(hash256_strategy()),
        proptest::option::of("[a-zA-Z0-9 .]{0,64}"),
    )
        .prop_map(
            |(
                seq,
                koid,
                version,
                kind_tag,
                origin,
                actor,
                commit_ts,
                payload_hash,
                prev_audit_hash,
                audit_hash,
                signature,
                note,
            )| {
                KnowledgeEvent {
                    seq,
                    koid,
                    version,
                    kind: EventKind::from_tag(kind_tag % 6).unwrap_or(EventKind::Created),
                    origin,
                    actor,
                    commit_ts,
                    payload_hash,
                    prev_audit_hash,
                    audit_hash,
                    signature,
                    note,
                }
            },
        )
}

proptest! {
    #[test]
    fn ko_codec_roundtrip(ko in knowledge_object_strategy()) {
        let bytes = encode_ko(&ko);
        let decoded = decode_ko(&bytes).expect("decode must succeed for valid KO");
        assert_eq!(ko, decoded);
    }

    #[test]
    fn ke_codec_roundtrip(ke in knowledge_event_strategy()) {
        let bytes = encode_ke(&ke);
        let decoded = decode_ke(&bytes).expect("decode must succeed for valid KE");
        assert_eq!(ke, decoded);
    }

    #[test]
    fn ko_encoding_is_canonical(ko in knowledge_object_strategy()) {
        assert_eq!(encode_ko(&ko), encode_ko(&ko));
    }

    #[test]
    fn ke_encoding_is_canonical(ke in knowledge_event_strategy()) {
        assert_eq!(encode_ke(&ke), encode_ke(&ke));
    }

    #[test]
    fn lifecycle_transition_matrix_is_deterministic(from in lifecycle_state_strategy(), to in lifecycle_state_strategy()) {
        let expected = matches!(
            (from, to),
            (LifecycleState::Draft, LifecycleState::Active)
                | (LifecycleState::Active, LifecycleState::Verified)
                | (LifecycleState::Verified, LifecycleState::Archived)
                | (LifecycleState::Archived, LifecycleState::Deleted)
        );
        assert_eq!(from.can_transition(to), expected);
    }

    #[test]
    fn idgen_is_monotonic(salt in any::<u64>(), timestamps in collection::vec(0..(1u64 << 48), 2..20)) {
        let mut gen = IdGen::new(salt);
        let mut last = gen.next(0);
        for t in timestamps {
            let next = gen.next(t);
            assert!(next >= last, "KOIDs must be monotonically non-decreasing");
            assert_ne!(next, last, "consecutive KOIDs must be unique");
            last = next;
        }
    }
}
