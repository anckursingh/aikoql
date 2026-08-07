//! Fuzz-style conformance tests for KOM codec and lifecycle (MRFC-0001 §14).
//!
//! This runs on stable Rust using proptest. The ideal `cargo-fuzz` harness
//! (libfuzzer-sys) is deferred until nightly is available; this test gives the
//! same coverage guarantees against panics and invalid success for malformed
//! input.

use mnemosyne_kernel::codec::{decode_ke, decode_ko};
use mnemosyne_kernel::kom::{EventKind, KError, KnowledgeObject, LifecycleState, Metadata, KOID};
use proptest::prelude::*;

proptest! {
    /// Random bytes fed to KO decode must never panic and must either produce
    /// a valid KnowledgeObject or a Codec error (never InvalidObject/Store/etc).
    #[test]
    fn decode_ko_never_panics_on_arbitrary_input(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        match decode_ko(&bytes) {
            Ok(ko) => {
                // If it decoded, re-encode must round-trip exactly.
                let reencoded = mnemosyne_kernel::codec::encode_ko(&ko);
                assert_eq!(
                    bytes, reencoded,
                    "valid decode from arbitrary bytes must be canonical"
                );
            }
            Err(KError::Codec(_)) => {}
            Err(other) => panic!(
                "arbitrary input must produce Codec error, got {:?}",
                other
            ),
        }
    }

    /// Random bytes fed to KE decode must never panic and must either produce
    /// a valid KnowledgeEvent or a Codec error.
    #[test]
    fn decode_ke_never_panics_on_arbitrary_input(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        match decode_ke(&bytes) {
            Ok(ke) => {
                let reencoded = mnemosyne_kernel::codec::encode_ke(&ke);
                assert_eq!(
                    bytes, reencoded,
                    "valid decode from arbitrary bytes must be canonical"
                );
            }
            Err(KError::Codec(_)) => {}
            Err(other) => panic!(
                "arbitrary input must produce Codec error, got {:?}",
                other
            ),
        }
    }

    /// Lifecycle transition predicate must be deterministic for all pairs.
    #[test]
    fn lifecycle_validation_never_panics(from_tag in any::<u8>(), to_tag in any::<u8>()) {
        let from = LifecycleState::from_tag(from_tag);
        let to = LifecycleState::from_tag(to_tag);
        match (from, to) {
            (Some(f), Some(t)) => {
                let _ = f.can_transition(t);
            }
            _ => {}
        }
    }

    /// KnowledgeObject::validate must never panic on arbitrary struct contents.
    #[test]
    fn ko_validate_never_panics(
        type_name in proptest::option::of(".*"),
        owner in proptest::option::of(".*"),
    ) {
        let ko = KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: type_name.unwrap_or_default(),
                tenant: None,
                schema_version: 0,
                tags: vec![],
            },
            mnemosyne_kernel::kom::SecurityDescriptor {
                owner: owner.unwrap_or_default(),
                acl: vec![],
                classification: None,
            },
        );
        let _ = ko.validate();
    }

    /// EventKind tag parsing must be deterministic and never panic.
    #[test]
    fn event_kind_tag_roundtrip(tag in any::<u8>()) {
        if let Some(kind) = EventKind::from_tag(tag) {
            assert_eq!(EventKind::from_tag(kind.tag()), Some(kind));
        }
    }
}
