//! AIKOQL v2 storage engine (SE2) — segmented WAL, immutable segments,
//! manifest. See docs/AIKOQL_Storage_Engine_V2_Production_Design.md and
//! docs/IMPLEMENTATION-PLAN-V2.md.
//!
//! SE2-M0: format contracts — CURRENT, manifest, checksums, atomic
//! publication. SE2-M1: immutable segments (writer + reader). SE2-M2:
//! WAL frames, memtable, flush, Db with durability modes and the OS lock.

pub mod db;
pub mod format;
pub mod memtable;
pub mod segment;
pub mod wal;
