//! D-12 RED: no public Rust client exists yet.
//!
//! `tests/conformance.rs` pins the shared runner's rust arm
//! (`scripts/sdk-conformance.sh --language rust`), which today answers
//! "not implemented yet (D-12..D-14)" with exit 2 — the milestone RED.
//! The GREEN is `crates/sdk/rust` as the reference implementation of the
//! canonical Database API (embedded + remote), executing the same 16
//! vectors / 72 ops every other SDK runs.
