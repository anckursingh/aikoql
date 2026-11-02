//! AG-30 — per-commit journal signatures. au30_001–002.
//!
//! Every journal event committed while the kernel holds an Ed25519 audit
//! signer carries a signature over (previous signature || its audit hash) —
//! a chain verifiable with the public key alone, where dropping, reordering
//! or tampering with any signed event breaks its successor. The chain's
//! tail IS the signed journal head (the evidence pack's audit_chain), so
//! the per-commit stream and the head signature attest the same journal.
//!
//! shortcut: events committed BEFORE the signer is attached (the catalog
//! bootstrap on a fresh open) are outside the stream — every commit after
//! the first signed one must sign, or the chain has a hole; tighten when a
//! signer is plumbed into `Kernel::open` itself.

use aikoql_kernel::security::{Signer, SigningKey};
use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use std::sync::Arc;

// --- helpers ------------------------------------------------------------------

fn mk() -> Kernel {
    Kernel::open(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
    )
    .unwrap()
}

fn alice() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn note(k: &Kernel, n: i64) -> KOID {
    let mut req = RememberRequest::create(alice(), Metadata {
        type_name: "note".into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    });
    req.properties.insert("n".into(), Value::Int(n));
    k.remember(req).unwrap().koid
}

/// Walk the journal, verifying the per-commit signature chain from the
/// public key alone. Returns (signed_count, tail_seq, tail_audit_hash).
fn verify_chain(k: &Kernel, public: &[u8; 32]) -> (usize, u64, [u8; 32]) {
    let mut prev = [0u8; 64];
    let mut seen_signed = false;
    let mut signed = 0usize;
    let mut tail_seq = 0u64;
    let mut tail_hash = [0u8; 32];
    for ke in k.journal().unwrap() {
        match ke.audit_signature {
            Some(sig) => {
                let mut msg = Vec::with_capacity(96);
                msg.extend_from_slice(&prev);
                msg.extend_from_slice(&ke.audit_hash);
                assert!(
                    SigningKey::verify(public, &msg, &sig),
                    "seq {} signature verifies",
                    ke.seq
                );
                prev = sig;
                seen_signed = true;
                signed += 1;
                tail_seq = ke.seq;
                tail_hash = ke.audit_hash;
            }
            None => assert!(
                !seen_signed,
                "seq {}: no unsigned event after the first signed one",
                ke.seq
            ),
        }
    }
    (signed, tail_seq, tail_hash)
}

// --- au30_001 — every committed event carries a chained signature ---------------

#[test]
fn au30_001_each_committed_event_carries_a_chained_signature() {
    let signer = Arc::new(Signer::new());
    signer.set_key(SigningKey::generate().unwrap());
    let public = signer.public_key().unwrap();
    let k = mk().with_audit_signer(signer);
    for i in 0..3 {
        note(&k, i);
    }
    let (signed, tail_seq, tail_hash) = verify_chain(&k, &public);
    assert!(signed >= 3, "the remembered events are signed");
    let (seq, audit) = k.journal_head().unwrap();
    assert_eq!(tail_seq, seq, "the signed tail is the journal head");
    assert_eq!(
        tail_hash, audit,
        "the per-commit chain ends in the signed audit chain"
    );

    // A tampered event hash breaks that event's signature.
    let ke = k
        .journal()
        .unwrap()
        .into_iter()
        .find(|ke| ke.audit_signature.is_some())
        .expect("a signed event");
    let sig = ke.audit_signature.unwrap();
    let mut msg = Vec::with_capacity(96);
    msg.extend_from_slice(&[0u8; 64]);
    msg.extend_from_slice(&ke.audit_hash);
    let mut tampered = msg;
    tampered[95] ^= 1;
    assert!(
        !SigningKey::verify(&public, &tampered, &sig),
        "a flipped hash byte fails the signature"
    );
}

// --- au30_002 — an unsigned kernel commits unsigned events -----------------------

#[test]
fn au30_002_an_unsigned_kernel_commits_unsigned_events() {
    let k = mk();
    note(&k, 1);
    assert!(
        k.journal()
            .unwrap()
            .iter()
            .all(|ke| ke.audit_signature.is_none()),
        "no signer, no per-commit signatures"
    );
}

// --- au30_003 — a signer plumbed into open signs the bootstrap -------------------

#[test]
fn au30_003_open_signed_signs_the_catalog_bootstrap() {
    let signer = Arc::new(Signer::new());
    signer.set_key(SigningKey::generate().unwrap());
    let public = signer.public_key().unwrap();
    let k = Kernel::open_signed(
        Arc::new(MemoryEngine::new()),
        Arc::new(ManualClock::new(10_000)),
        0xC0FFEE,
        signer,
    )
    .unwrap();
    let events = k.journal().unwrap();
    assert_eq!(events[0].seq, 1, "the bootstrap is the first journal event");
    assert!(
        events.iter().all(|ke| ke.audit_signature.is_some()),
        "every event is signed, the bootstrap included"
    );
    let (signed, tail_seq, tail_hash) = verify_chain(&k, &public);
    assert_eq!(signed, events.len(), "the signed stream covers the whole journal");
    let (seq, audit) = k.journal_head().unwrap();
    assert_eq!(tail_seq, seq, "the signed tail is the journal head");
    assert_eq!(tail_hash, audit, "the chain ends in the signed audit chain");

    // the chain's first link is the bootstrap itself, anchored at zeros
    let first = &events[0];
    let sig = first.audit_signature.unwrap();
    let mut msg = Vec::with_capacity(96);
    msg.extend_from_slice(&[0u8; 64]);
    msg.extend_from_slice(&first.audit_hash);
    assert!(
        SigningKey::verify(&public, &msg, &sig),
        "seq 1 signs from the zero anchor"
    );
}
