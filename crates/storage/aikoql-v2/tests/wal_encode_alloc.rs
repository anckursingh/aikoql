//! PERF-4 — `encode_frame` must build the frame in ONE allocation.
//! Today it builds a `payload` Vec (grown from 0, so several allocations)
//! and then a second `frame` Vec that copies it — every group-commit append
//! pays this. The fix computes the exact encoded length and writes straight
//! into one `Vec::with_capacity` (the `encoded_len` idiom the directory
//! records already use). One test in its own binary: the global-allocator
//! counter is process-wide, and a lone test means no sibling test thread
//! skews the count.

use aikoql_storage_v2::identity::{LogicalId, ObjectId, ReplicaId};
use aikoql_storage_v2::wal::{decode_frame, encode_frame, Op};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) == 1 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn encode_frame_allocates_exactly_once() {
    // One of every op — the op_encoded_len switch must cover all five arms
    // (an under-counted arm reallocs and the pin fails).
    let ops = [
        Op::Put(b"key-put".to_vec(), vec![0u8; 64]),
        Op::Delete(b"key-del".to_vec()),
        Op::CreateObject {
            oid: ObjectId::from_bytes([1; 16]),
            lid: LogicalId::from_bytes([2; 8]),
            rid: ReplicaId::from_bytes([3; 8]),
            pgen: 7,
        },
        Op::PutObject(
            ReplicaId::from_bytes([4; 8]),
            b"key-putobj".to_vec(),
            vec![0u8; 32],
        ),
        Op::DeleteObject(ReplicaId::from_bytes([5; 8]), b"key-delobj".to_vec()),
    ];
    ARMED.store(1, Ordering::Relaxed);
    let frame = encode_frame(1, &ops).unwrap();
    ARMED.store(0, Ordering::Relaxed);
    let allocs = ALLOCS.load(Ordering::Relaxed);
    assert_eq!(
        allocs, 1,
        "encode_frame allocated {allocs} blocks — the payload Vec is built \
         separately (and re-grows) before being copied into the frame Vec; \
         one with_capacity of the final size must suffice"
    );

    // The pin must not pass by breaking the encoding: byte-exact round-trip.
    let (decoded, len) = decode_frame(&frame).unwrap();
    assert_eq!(len, frame.len());
    assert_eq!(decoded.ops, ops);
}
