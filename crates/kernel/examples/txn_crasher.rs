//! P5-M10 tx006 child: begin → stage → commit under a park window.
//!
//! Usage: `txn_crasher <db path> <txn id>`
//!
//! The commit parks at the stage named by `AIKOQL_TXN_PARK` (`pre_commit` —
//! after the batch is assembled, before the engine write — or `post_commit`
//! — after the engine write) and writes the `AIKOQL_TXN_PARK_MARKER` file so
//! the parent knows the window was reached, then sleeps forever. The parent
//! hard-kills it and checks the store.

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 3, "usage: txn_crasher <db path> <txn id>");
    let engine: Arc<dyn StorageEngine> = Arc::new(
        aikoql_storage_v2::AikoqlStorageEngineV2::open(std::path::Path::new(&args[1]))
            .expect("open db"),
    );
    let k = Kernel::open(engine, Arc::new(SystemClock), 0xBEEF).expect("open kernel");
    let mut t = k
        .begin_transaction(Subject::new("alice"), args[2].clone())
        .expect("begin");
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("alice")),
        Metadata {
            type_name: "Node".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties.insert("i".into(), Value::Int(7));
    t.stage(req).expect("stage");
    let (r, _) = t.commit().expect("commit");
    // Reached only when no park fired (a plain run) — for manual checks.
    println!("committed {} result(s), version {}", r.len(), r[0].version);
}
