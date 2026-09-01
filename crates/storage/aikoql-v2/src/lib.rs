//! AIKOQL v2 storage engine (SE2) — segmented WAL, immutable segments,
//! manifest. See docs/AIKOQL_Storage_Engine_V2_Production_Design.md and
//! docs/IMPLEMENTATION-PLAN-V2.md.
//!
//! SE2-M0: format contracts — CURRENT, manifest, checksums, atomic
//! publication. SE2-M1: immutable segments (writer + reader). SE2-M2:
//! WAL frames, memtable, flush, Db with durability modes and the OS lock.
//! SE2-M3: bounded recovery — replay only the active WAL, orphan/missing
//! segment policies, legacy v1 WAL migration (§23). SE2-M4: L0 → L1
//! compaction — synchronous k-way merge, newest-per-key wins. SE2-M5:
//! retention policy as a compaction input — KEEP/DROP/ARCHIVE per key
//! class, `compact_with(policy)`, `compact()` = KeepAll. SE2-M6: group
//! commit — a committer thread drains `Db::writer()` handles into groups
//! (one fsync per group, apply-before-ack, exact-fit caps); Sync remains
//! the correctness baseline.

pub mod compaction;
pub mod db;
pub mod format;
pub mod memtable;
pub mod migration;
pub mod segment;
pub mod wal;
