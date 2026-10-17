//! Prove isolation (device-eval N3): `prove` must see a quiescent journal.
//!
//! The walk scans the event rows snapshot-less and then compares the chain
//! tail against `journal_head` — an append between the two used to make an
//! untampered chain report `chain_valid: false` (the semantic engine's
//! catch-up appends continuously, so proves raced it). `prove` now holds the
//! pipe lock, so concurrent writers block and every prove is point-in-time.

use aikoql_kernel::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Delegating engine whose event-prefix scans sleep, widening the window
/// between the scan snapshot and the journal-head read to 50 ms — long
/// enough that a concurrent writer is guaranteed to append inside it.
struct SlowScanEngine {
    inner: Arc<MemoryEngine>,
}

impl StorageEngine for SlowScanEngine {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }

    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        if prefix == b"ke/" {
            thread::sleep(Duration::from_millis(50));
        }
        self.inner.scan(prefix)
    }

    fn write_batch(&self, batch: &WriteBatch) -> KResult<()> {
        self.inner.write_batch(batch)
    }
}

fn mk() -> Kernel {
    let clock = Arc::new(ManualClock::new(10_000));
    let engine = Arc::new(SlowScanEngine {
        inner: Arc::new(MemoryEngine::new()),
    });
    Kernel::open(engine, clock, 0xBEEF).unwrap()
}

#[test]
fn prove_stays_valid_while_a_writer_appends() {
    let k = mk();
    let alice = Subject::new("alice");
    let mut req = RememberRequest::create(
        alice.clone(),
        Metadata {
            type_name: "fact".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text("x".into()));
    let id = k.remember(req).unwrap().koid;

    // Writer: append versions continuously while the prove's scan sleeps.
    let stop = Arc::new(AtomicBool::new(false));
    let k2 = k.clone_handle();
    let stop2 = stop.clone();
    let w_alice = alice.clone();
    let writer = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            let _ = k2.remember(RememberRequest::update(
                w_alice.clone(),
                id,
                Metadata {
                    type_name: "fact".into(),
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
            ));
            thread::sleep(Duration::from_millis(1));
        }
    });

    // The prove's event scan sleeps 50 ms with the writer appending every
    // millisecond: without the pipe lock the head check reads a journal
    // that has advanced past the scanned tail and reports a false break.
    let proof = k.prove(alice.clone(), &id).unwrap();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    assert!(
        proof.chain_valid,
        "prove raced the concurrent writer ({} events) — the journal was \
         untampered, the walk must be quiescent-safe",
        proof.events
    );
}
