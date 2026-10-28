//! MRFC-0080 standby reads — a kernel over a store another writer (the
//! replication stream) fills. It must open, subscribe and catch up
//! without writing a single row, and see what the writer committed.

use aikoql_kernel::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The standby's view: reads pass through, every write is refused (and
/// counted) — as on a fenced replica.
struct ReadOnly {
    inner: Arc<MemoryEngine>,
    writes: AtomicUsize,
}

impl StorageEngine for ReadOnly {
    fn get(&self, key: &[u8]) -> KResult<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn scan(&self, prefix: &[u8]) -> KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.inner.scan(prefix)
    }
    fn write_batch(&self, _batch: &WriteBatch) -> KResult<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(KError::Store("standby: read only".into()))
    }
}

fn note(k: &Kernel, n: i64) -> KOID {
    let mut req = RememberRequest::create(
        Subject::new("alice"),
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert("n".into(), Value::Int(n));
    k.remember(req).unwrap().koid
}

#[test]
fn a_standby_kernel_opens_without_writing_and_catches_up() {
    let mem = Arc::new(MemoryEngine::new());
    let leader = Kernel::open(mem.clone(), Arc::new(SystemClock), 1).unwrap();
    let first = note(&leader, 1);

    let ro = Arc::new(ReadOnly {
        inner: mem.clone(),
        writes: AtomicUsize::new(0),
    });
    let standby = Kernel::open_standby(ro.clone(), Arc::new(SystemClock), 2).unwrap();
    let events = standby.notify(EventFilter::default()).unwrap();
    assert_eq!(ro.writes.load(Ordering::SeqCst), 0, "open/subscribe wrote");
    let alice = Subject::new("alice");
    assert!(
        standby.get(&alice, &first).is_ok(),
        "the replicated object reads"
    );

    let second = note(&leader, 2); // the stream applies it underneath
    let head = standby.catch_up().unwrap();
    assert_eq!(head, leader.journal_head().unwrap().0);
    let ev = events
        .recv_timeout(Duration::from_secs(1))
        .expect("catch_up broadcast the replicated event");
    assert_eq!(ev.koid, second);
    // reads snapshot at the kernel's clock: it must cover what it caught up to
    assert!(
        standby.snapshot() >= ev.commit_ts,
        "the standby's read snapshot {} predates the replicated commit {}",
        standby.snapshot(),
        ev.commit_ts
    );
    assert_eq!(
        standby.get(&alice, &second).unwrap().properties.get("n"),
        Some(&Value::Int(2))
    );
    assert_eq!(
        standby.catch_up().unwrap(),
        head,
        "nothing new, nothing re-broadcast"
    );
    assert!(events.try_recv().is_err());
    assert_eq!(ro.writes.load(Ordering::SeqCst), 0, "catch_up wrote");
}
