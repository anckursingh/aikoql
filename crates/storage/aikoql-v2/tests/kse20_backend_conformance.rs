//! KSE-20 backend conformance (MRFC-KSE-001 §26) — v2-only since the
//! launch S-02 decommission (the v1/redb/memory matrix died with the
//! deprecated backends; the six §7 asserts survive as the engine's own
//! conformance pin, the one shared definition in `common::kse`, copied
//! verbatim from v1's harness).

mod common;

use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use aikoql_storage_v2::AikoqlStorageEngineV2;
use common::{kse, run_date, tmp};
use std::path::PathBuf;

/// All six §7 asserts — any divergence panics, so a ✓ row in the report is
/// honest by construction (the report is only written when everything
/// passed).
fn run_six(e: &dyn StorageEngine) {
    kse::kse001_get(e);
    kse::kse002_missing_key(e);
    kse::kse003_prefix_scan(e);
    kse::kse004_atomic_batch(e);
    kse::kse005_empty_batch(e);
    kse::kse006_conflicting_put_delete(e);
}

#[test]
fn kse20_backend_conformance_v2() {
    let path = tmp("kse20v2-v2");
    let engine = Box::new(AikoqlStorageEngineV2::open(&path).unwrap());
    run_six(engine.as_ref());

    // Durability probe: write → drop the handle → reopen → read.
    let mut w = WriteBatch::new();
    w.put(b"kse20-reopen".to_vec(), b"v".to_vec());
    engine.write_batch(&w).unwrap();
    drop(engine);
    let reopened = Box::new(AikoqlStorageEngineV2::open(&path).unwrap());
    assert_eq!(
        reopened.get(b"kse20-reopen").unwrap(),
        Some(b"v".to_vec()),
        "state lost across reopen"
    );

    let date = run_date();
    let report = format!(
        "# Backend Conformance — v2 (MRFC-KSE-001 §7 + §26)\n\n\
         Date: {date} · the six KSE-1 asserts from one shared definition \
         (`tests/common` `kse` module, copied verbatim from v1's harness), run \
         against the engine through `&dyn StorageEngine` — no engine-specific \
         type above the boundary (§32). The pre-S-02 cross-backend matrix \
         (memory/redb/aikoql/aikoql-v2) died with the decommission; the six \
         asserts and the reopen probe remain the conformance pin.\n\n\
         | backend | KSE-001..006 | persistence (reopen) | physical format | read path |\n\
         |---|---|---|---|---|\n\
         | aikoql-v2 | 6/6 ✓ | reopen ✓ | bounded WAL + immutable segments + \
         manifest (dir) | memtable + segment readers (bloom-skipped, \
         block-cached) |\n\n\
         ## Capabilities (documented, engine-specific)\n\n\
         - durability: fsyncs every Sync batch (pinned by the SE2-M2 WAL \
         goldens, the M3/M4/M6 child-kill recovery suites); GroupCommit \
         mode (committer thread, one fsync per group — SE2-M6) behind the \
         same Sync baseline.\n\
         - read path: memtable first and, per segment, seeks by index, \
         skips via the bloom pre-check, and caches decoded blocks within \
         `cache_bytes` (SE2-M7).\n\
         - concurrency: serializes writes at the engine boundary.\n",
    );
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../artifacts/storage-engine-v2");
    common::report_write(&dir.join("conformance.md"), report);
}
