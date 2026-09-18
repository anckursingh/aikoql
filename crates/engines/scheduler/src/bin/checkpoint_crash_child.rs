//! P5-M27 (IDX-P0-01) crash harness: a persistent aikoql-v2 store, two
//! property indexes, seeded rows, a started maintainer, and a checkpoint
//! parked at a named stage (the CHECKPOINT_PARK_* env hooks). The
//! crash-matrix tests spawn this and either kill it inside the window or
//! let its writer thread release the park. Args: <db_dir> <ckpt_dir>
//! <stage> [release].

use aikoql_kernel::transaction::kernel::{KnowledgeContext, RememberRequest, Subject};
use aikoql_kernel::*;
use aikoql_scheduler::{IndexMaintainer, SchedulerJob};
use aikoql_storage_v2::AikoqlStorageEngineV2;
use std::sync::Arc;
use std::time::Duration;

fn note(k: &Kernel, body: &str, tag: &str) -> KOID {
    let mut req = RememberRequest::create(
        KnowledgeContext::new(Subject::new("crash-child")),
        Metadata {
            type_name: "note".into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        },
    );
    req.properties
        .insert("body".into(), Value::Text(body.into()));
    req.properties.insert("tag".into(), Value::Text(tag.into()));
    k.remember(req).unwrap().koid
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db = &args[1];
    let ckpt = &args[2];
    let stage = &args[3];
    let release = args.get(4).is_some();

    std::env::set_var("CHECKPOINT_PARK_AT", stage);
    std::env::set_var("CHECKPOINT_PARK_ACK", format!("{ckpt}.ack"));
    std::env::set_var("CHECKPOINT_PARK_RELEASE", format!("{ckpt}.release"));

    let k = Kernel::open(
        Arc::new(AikoqlStorageEngineV2::open(db).unwrap()),
        Arc::new(SystemClock),
        0xA9C9,
    )
    .unwrap();
    k.catalog_create_index("by_body", "note", &["body"])
        .unwrap();
    k.catalog_create_index("by_tag", "note", &["tag"]).unwrap();
    for i in 0..10 {
        note(&k, &format!("seed-{i:02}"), "group-a");
    }

    let v: Arc<dyn VectorIndex> = Arc::new(BruteForceVectorIndex::new());
    let t: Arc<dyn TextIndex> = Arc::new(TokenTextIndex::new());
    let m = Arc::new(IndexMaintainer::new(v, t));
    SchedulerJob::start(&*m, &k).unwrap();
    m.wait_caught_up(&k, Duration::from_secs(30)).unwrap();

    // The overlap writer: a commit that lands while the checkpoint is
    // parked. `.late-committed` is written after the journal append, so a
    // test-side kill never loses it; in release mode the park is then
    // released and the funnel completes.
    let k2 = k.clone_handle();
    let ckpt2 = ckpt.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        note(&k2, "late-into-the-window", "group-b");
        std::fs::write(format!("{ckpt2}.late-committed"), b"1").unwrap();
        if release {
            std::fs::write(format!("{ckpt2}.release"), b"1").unwrap();
        }
    });

    // The publication window only exists once a previous checkpoint is in
    // place: publish-kill runs the funnel twice, the test releasing the
    // first park and killing inside the remove-then-rename window of the
    // second.
    let rounds = if stage == "publish" && !release { 2 } else { 1 };
    for _ in 0..rounds {
        m.checkpoint(&k, std::path::Path::new(ckpt)).unwrap();
        // Consume the release: a later park (publish-kill's second funnel)
        // must wait for a fresh one, not pass through on a stale file.
        let _ = std::fs::remove_file(format!("{ckpt}.release"));
    }
    writer.join().unwrap();
    std::fs::write(format!("{ckpt}.done"), b"1").unwrap();
}
