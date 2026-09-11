//! SE2-M8 — read-path instrumentation (QA spec M0): cumulative atomics on
//! the Db and its SegmentReaders. Counters move only with real operations;
//! timings run only when stats are attached (readers opened directly carry
//! None — zero overhead in the format/golden suites).
//!
//! Counter scope, by owner: the Db-level counters (`lookups`,
//! `memtable_*`, `segments_*`) count only `Db::get` — the W1/W2 diagnosis
//! they exist for. The SegmentReader-level counters (`block_*`, `index_*`,
//! `bytes_read`, `entries_decoded`) count every block load that reader
//! serves — scans and compaction pulls included. That is the honest split:
//! a compaction's I/O is not a point read.

use std::sync::atomic::{AtomicU64, Ordering};

/// P3-M2 (design §21) — fsync-latency histogram edges in µs. Twelve
/// buckets: everything above 16 ms lands in the last one.
pub const FSYNC_LATENCY_BUCKETS: usize = 12;
const FSYNC_LATENCY_EDGES: [u64; FSYNC_LATENCY_BUCKETS - 1] =
    [16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384];

/// Record one latency sample into its bucket (first edge it fits under;
/// past the last edge = the final bucket). Relaxed — the counters are
/// advisory, never a correctness input.
pub(crate) fn record_latency_us(buckets: &[AtomicU64; FSYNC_LATENCY_BUCKETS], us: u64) {
    let idx = FSYNC_LATENCY_EDGES
        .iter()
        .position(|&e| us <= e)
        .unwrap_or(FSYNC_LATENCY_BUCKETS - 1);
    buckets[idx].fetch_add(1, Ordering::Relaxed);
}

/// A snapshot of the cumulative read-path counters (the QA doc's
/// `ReadPathMetrics`; `value_decode_ns` is folded into `block_decode_ns` —
/// values decode with their entries, a separate counter would be fiction).
/// `segments_range_skipped` fires before the bloom probe (SE2-M9).
/// SE2-M21 adds the attribution closure: `lock_wait_ns` (state-guard wait),
/// `bloom_probe_ns` (the bloom pre-check — untimed it would be ~a quarter
/// of a warm cache hit, so the accounting could not close), `get_wall_ns`
/// (the whole get — the denominator the residual is bounded against).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReadPathStats {
    pub lookups: u64,
    pub memtable_lookup_ns: u64,
    pub memtable_hits: u64,
    pub segments_considered: u64,
    pub segments_range_skipped: u64,
    pub segments_bloom_skipped: u64,
    pub segments_index_searched: u64,
    pub index_lookup_ns: u64,
    pub block_cache_lookup_ns: u64,
    pub block_cache_hits: u64,
    pub block_cache_misses: u64,
    pub block_io_ns: u64,
    pub block_decode_ns: u64,
    pub blocks_read: u64,
    pub bytes_read: u64,
    pub entries_decoded: u64,
    pub lock_wait_ns: u64,
    pub bloom_probe_ns: u64,
    pub get_wall_ns: u64,
}

/// The live counters — one per field, relaxed atomics (~ns overhead).
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub(crate) lookups: AtomicU64,
    pub(crate) memtable_lookup_ns: AtomicU64,
    pub(crate) memtable_hits: AtomicU64,
    pub(crate) segments_considered: AtomicU64,
    pub(crate) segments_range_skipped: AtomicU64,
    pub(crate) segments_bloom_skipped: AtomicU64,
    pub(crate) segments_index_searched: AtomicU64,
    pub(crate) index_lookup_ns: AtomicU64,
    pub(crate) block_cache_lookup_ns: AtomicU64,
    pub(crate) block_cache_hits: AtomicU64,
    pub(crate) block_cache_misses: AtomicU64,
    pub(crate) block_io_ns: AtomicU64,
    pub(crate) block_decode_ns: AtomicU64,
    pub(crate) blocks_read: AtomicU64,
    pub(crate) bytes_read: AtomicU64,
    pub(crate) entries_decoded: AtomicU64,
    pub(crate) lock_wait_ns: AtomicU64,
    pub(crate) bloom_probe_ns: AtomicU64,
    pub(crate) get_wall_ns: AtomicU64,
}

impl Stats {
    pub(crate) fn snapshot(&self) -> ReadPathStats {
        ReadPathStats {
            lookups: self.lookups.load(Ordering::Relaxed),
            memtable_lookup_ns: self.memtable_lookup_ns.load(Ordering::Relaxed),
            memtable_hits: self.memtable_hits.load(Ordering::Relaxed),
            segments_considered: self.segments_considered.load(Ordering::Relaxed),
            segments_range_skipped: self.segments_range_skipped.load(Ordering::Relaxed),
            segments_bloom_skipped: self.segments_bloom_skipped.load(Ordering::Relaxed),
            segments_index_searched: self.segments_index_searched.load(Ordering::Relaxed),
            index_lookup_ns: self.index_lookup_ns.load(Ordering::Relaxed),
            block_cache_lookup_ns: self.block_cache_lookup_ns.load(Ordering::Relaxed),
            block_cache_hits: self.block_cache_hits.load(Ordering::Relaxed),
            block_cache_misses: self.block_cache_misses.load(Ordering::Relaxed),
            block_io_ns: self.block_io_ns.load(Ordering::Relaxed),
            block_decode_ns: self.block_decode_ns.load(Ordering::Relaxed),
            blocks_read: self.blocks_read.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            entries_decoded: self.entries_decoded.load(Ordering::Relaxed),
            lock_wait_ns: self.lock_wait_ns.load(Ordering::Relaxed),
            bloom_probe_ns: self.bloom_probe_ns.load(Ordering::Relaxed),
            get_wall_ns: self.get_wall_ns.load(Ordering::Relaxed),
        }
    }
}

// ---------------------------------------------------------------------------
// P4-M5 — the per-request read trace. One record per read request when
// sampling fires: the delta of the cumulative ReadPathStats across the
// request. Value-opaque (no user bytes leave the engine) and zero-cost when
// disabled (the wrapper never snapshots, never locks). Deltas are
// best-effort under concurrent readers — counters interleave, so a record
// attributes its request, it never gates an answer.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTraceRecord {
    pub seq: u64,
    pub wall_ns: u64,
    pub lock_wait_ns: u64,
    pub memtable_lookup_ns: u64,
    pub memtable_hits: u64,
    pub segments_considered: u64,
    pub segments_range_skipped: u64,
    pub segments_bloom_skipped: u64,
    pub segments_index_searched: u64,
    pub index_lookup_ns: u64,
    pub block_cache_lookup_ns: u64,
    pub block_cache_hits: u64,
    pub block_cache_misses: u64,
    pub block_io_ns: u64,
    pub block_decode_ns: u64,
    pub blocks_read: u64,
    pub bytes_read: u64,
    pub entries_decoded: u64,
    pub bloom_probe_ns: u64,
    /// True when the request needed no new block reads (block cache or
    /// memtable served it).
    pub cache_hit: bool,
}

impl ReadPathStats {
    /// P4-M5 — per-request delta against a before-snapshot. `seq` is 0
    /// here; the trace assigns the real request sequence on record.
    pub fn delta_of(&self, before: &ReadPathStats) -> ReadTraceRecord {
        macro_rules! d {
            ($f:ident) => {
                self.$f.saturating_sub(before.$f)
            };
        }
        ReadTraceRecord {
            seq: 0,
            wall_ns: d!(get_wall_ns),
            lock_wait_ns: d!(lock_wait_ns),
            memtable_lookup_ns: d!(memtable_lookup_ns),
            memtable_hits: d!(memtable_hits),
            segments_considered: d!(segments_considered),
            segments_range_skipped: d!(segments_range_skipped),
            segments_bloom_skipped: d!(segments_bloom_skipped),
            segments_index_searched: d!(segments_index_searched),
            index_lookup_ns: d!(index_lookup_ns),
            block_cache_lookup_ns: d!(block_cache_lookup_ns),
            block_cache_hits: d!(block_cache_hits),
            block_cache_misses: d!(block_cache_misses),
            block_io_ns: d!(block_io_ns),
            block_decode_ns: d!(block_decode_ns),
            blocks_read: d!(blocks_read),
            bytes_read: d!(bytes_read),
            entries_decoded: d!(entries_decoded),
            bloom_probe_ns: d!(bloom_probe_ns),
            cache_hit: self.blocks_read == before.blocks_read,
        }
    }
}

// ---------------------------------------------------------------------------
// P3-M2 (design §21) — write-path instrumentation. Same discipline as the
// read path: cumulative relaxed atomics that move only with real
// operations; a snapshot struct for callers. The gauges
// (compaction_backlog_bytes / compaction_pending_segments) are L0-only —
// the backlog a background compactor would have to drain — and are
// refreshed by the maybe_compact scan on every write path plus after every
// successful merge.
// ---------------------------------------------------------------------------

/// A snapshot of the write-path counters (design §21's `WritePathStats`).
/// `fsync_count` rides the Db's existing commit-fsync counter (one per
/// batch/group; flush truncation syncs are not counted).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WritePathStats {
    /// WAL frame bytes appended since open — cumulative, flushes truncate
    /// the file, not the ledger.
    pub wal_bytes: u64,
    pub flush_count: u64,
    pub flush_latency_us: u64,
    pub fsync_count: u64,
    pub fsync_latency_us_buckets: [u64; FSYNC_LATENCY_BUCKETS],
    /// Σ uncompacted L0 segment bytes / count while the trigger is
    /// unsatisfied — the backlog a background compactor (P3-M8) would
    /// drain.
    pub compaction_backlog_bytes: u64,
    pub compaction_pending_segments: u64,
    pub checkpoint_count: u64,
    pub checkpoint_latency_us: u64,
    pub write_queue_depth: u64,
    pub group_commit_batches: u64,
    pub group_commit_ops: u64,
    pub group_commit_max_ops: u64,
    /// Wall ms of the last merge that actually ran (0 = none yet).
    pub last_compaction_ms: u64,
    /// This open's recovery: wall ms of open() and WAL bytes replayed.
    pub recovery_ms: u64,
    pub wal_replay_bytes: u64,
}

/// The live write-path counters.
#[derive(Debug, Default)]
pub(crate) struct WriteStats {
    pub(crate) wal_bytes: AtomicU64,
    pub(crate) flush_count: AtomicU64,
    pub(crate) flush_latency_us: AtomicU64,
    pub(crate) fsync_latency_us_buckets: [AtomicU64; FSYNC_LATENCY_BUCKETS],
    pub(crate) compaction_backlog_bytes: AtomicU64,
    pub(crate) compaction_pending_segments: AtomicU64,
    pub(crate) checkpoint_count: AtomicU64,
    pub(crate) checkpoint_latency_us: AtomicU64,
    pub(crate) write_queue_depth: AtomicU64,
    pub(crate) group_commit_batches: AtomicU64,
    pub(crate) group_commit_ops: AtomicU64,
    pub(crate) group_commit_max_ops: AtomicU64,
    pub(crate) last_compaction_ms: AtomicU64,
    pub(crate) recovery_ms: AtomicU64,
    pub(crate) wal_replay_bytes: AtomicU64,
}

impl WriteStats {
    pub(crate) fn snapshot(&self, fsync_count: u64) -> WritePathStats {
        WritePathStats {
            wal_bytes: self.wal_bytes.load(Ordering::Relaxed),
            flush_count: self.flush_count.load(Ordering::Relaxed),
            flush_latency_us: self.flush_latency_us.load(Ordering::Relaxed),
            fsync_count,
            fsync_latency_us_buckets: std::array::from_fn(|i| {
                self.fsync_latency_us_buckets[i].load(Ordering::Relaxed)
            }),
            compaction_backlog_bytes: self.compaction_backlog_bytes.load(Ordering::Relaxed),
            compaction_pending_segments: self.compaction_pending_segments.load(Ordering::Relaxed),
            checkpoint_count: self.checkpoint_count.load(Ordering::Relaxed),
            checkpoint_latency_us: self.checkpoint_latency_us.load(Ordering::Relaxed),
            write_queue_depth: self.write_queue_depth.load(Ordering::Relaxed),
            group_commit_batches: self.group_commit_batches.load(Ordering::Relaxed),
            group_commit_ops: self.group_commit_ops.load(Ordering::Relaxed),
            group_commit_max_ops: self.group_commit_max_ops.load(Ordering::Relaxed),
            last_compaction_ms: self.last_compaction_ms.load(Ordering::Relaxed),
            recovery_ms: self.recovery_ms.load(Ordering::Relaxed),
            wal_replay_bytes: self.wal_replay_bytes.load(Ordering::Relaxed),
        }
    }
}

/// The segment inventory as of the snapshot: every manifest segment, all
/// levels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SegmentStats {
    pub count: u64,
    pub bytes: u64,
}

/// `Db::stats()` — the whole observable surface in one snapshot (design
/// §21's list, plus the read path and cache from SE2-M7/M8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DbStats {
    pub read: ReadPathStats,
    pub write: WritePathStats,
    pub segments: SegmentStats,
    pub cache: crate::cache::CacheStats,
}
