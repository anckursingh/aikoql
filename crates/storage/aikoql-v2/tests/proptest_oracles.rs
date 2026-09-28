//! L-19 (TDD-029) — proptest model oracles across the four storage layers.
//!
//! Every prior layer pin (merged_scan SE2-M12, boundary_matrix L-12,
//! prefix_scan_oracle L-18) fed FIXED or seeded-LCG op sequences. The gap:
//! no RANDOM-op-sequence oracle — a model (one map per surface) applied
//! alongside the engine, equality checked at layer transitions. proptest
//! shrinks any divergence to a minimal failing case and persists it under
//! proptest-regressions/, so a caught bug reproduces forever.
//!
//! Four properties, one per layer:
//! - memtable: random churn with NO flush — every read answers from the
//!   memtable alone;
//! - segments: flush every N ops while churn continues — reads merge
//!   memtable + immutables + segments; the final check rides a reopen
//!   (segments only);
//! - compaction: a multi-segment set merged by compact(); the L-04 class
//!   rides along — object rows survive the merge, never answer byte scans
//!   — checked before and after reopen;
//! - WAL: the crash-child pattern (resource_adversarial.rs) — the child
//!   applies the case and exits WITHOUT Db::drop (the graceful drop would
//!   flush the memtable out of the WAL), so the WAL is the rows' only
//!   copy; the parent's reopen replays it and checks equality. The child
//!   records its object ids in the shared dir — ids are assigned per
//!   session, the parent cannot know them.
//!
//! Default cases = proptest's 256 (the WAL leg pins 96: one process spawn
//! per case). The benchmark-nightly tier elevates the other three via
//! PROPTEST_CASES; any failure's regression file gets committed.
//!
//! No counting allocator here (behavioral oracles only — the alloc pins
//! live in allocation_scaling/prefix_scan_oracle), so no TEST_LOCK.

mod common;

use aikoql_storage_v2::db::{Config, Db, DurabilityMode};
use aikoql_storage_v2::identity::ObjectId;
use common::dir;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// No flush surprises, no background merge (l0_compact_trigger 0) — the
/// layer under test is the only thing that moves rows.
fn open_big(d: &Path) -> Db {
    let mut cfg = Config::new(d.to_path_buf());
    cfg.memtable_bytes = 1 << 30;
    cfg.l0_compact_trigger = 0;
    cfg.durability = DurabilityMode::Async;
    Db::open(cfg).unwrap()
}

fn new_session(d: &Path) -> (Db, [ObjectId; 3]) {
    let db = open_big(d);
    let oids = [
        db.create_object().unwrap(),
        db.create_object().unwrap(),
        db.create_object().unwrap(),
    ];
    (db, oids)
}

#[derive(Debug, Clone)]
enum Op {
    BytePut(Vec<u8>, Vec<u8>),
    ByteDel(Vec<u8>),
    ObjPut(usize, Vec<u8>, Vec<u8>),
}

/// The byte surface per key (None = tombstone) plus one map per object
/// surface. Object puts never touch the byte model — a scan must equal
/// exactly this, and object rows must answer only through their surface.
#[derive(Default)]
struct Model {
    bytes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    objs: [BTreeMap<Vec<u8>, Vec<u8>>; 3],
}

fn apply_db(db: &Db, op: &Op, oids: &[ObjectId; 3]) {
    match op {
        Op::BytePut(k, v) => {
            db.put(k, v).unwrap();
        }
        Op::ByteDel(k) => {
            db.delete(k).unwrap();
        }
        Op::ObjPut(oi, k, v) => {
            db.put_object(oids[*oi], k, v).unwrap();
        }
    }
}

fn apply_model(model: &mut Model, op: &Op) {
    match op {
        Op::BytePut(k, v) => {
            model.bytes.insert(k.clone(), Some(v.clone()));
        }
        Op::ByteDel(k) => {
            model.bytes.insert(k.clone(), None);
        }
        Op::ObjPut(oi, k, v) => {
            model.objs[*oi].insert(k.clone(), v.clone());
        }
    }
}

fn check_model(db: &Db, model: &Model, oids: &[ObjectId; 3], tag: &str) {
    let want: Vec<(Vec<u8>, Vec<u8>)> = model
        .bytes
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k.clone(), v.clone())))
        .collect();
    assert_eq!(
        db.scan(b"").unwrap(),
        want,
        "{tag}: full-range scan diverged"
    );
    for (oi, rows) in model.objs.iter().enumerate() {
        for (k, v) in rows {
            assert_eq!(
                db.get_object(oids[oi], k).unwrap().as_deref(),
                Some(v.as_slice()),
                "{tag}: object {oi} row for {k:?} diverged"
            );
        }
    }
}

fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Fixed alphabet — collisions, churn, keys that prefix each other.
        8 => (0..24u8).prop_map(|i| format!("k{i:02}").into_bytes()),
        // Arbitrary bytes — non-UTF8 and length drift (1..16; the length-0
        // boundary belongs to boundary_matrix L-12, not this oracle).
        2 => proptest::collection::vec(any::<u8>(), 1..16),
    ]
}

fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        8 => proptest::collection::vec(any::<u8>(), 1..96),
        1 => proptest::collection::vec(any::<u8>(), 97..512),
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (key_strategy(), value_strategy()).prop_map(|(k, v)| Op::BytePut(k, v)),
        2 => key_strategy().prop_map(Op::ByteDel),
        2 => (0..3usize, key_strategy(), value_strategy())
            .prop_map(|(oi, k, v)| Op::ObjPut(oi, k, v)),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(op_strategy(), 32..96)
}

// ---------------------------------------------------------------------------
// The crash-child case codec — hex lines, one op per line, in the shared
// dir (a file beats an env var: no 32 KB env-block ceiling, no quoting).
// ---------------------------------------------------------------------------

const CHILD_ENV: &str = "AIKOQL_V2_PROPTEST_CHILD";
const DIR_ENV: &str = "AIKOQL_V2_PROPTEST_DIR";
const OPS_FILE: &str = "proptest-case.ops";
const OIDS_FILE: &str = "proptest-case.oids";

fn encode_ops(ops: &[Op]) -> String {
    let mut s = String::new();
    for op in ops {
        match op {
            Op::BytePut(k, v) => s.push_str(&format!("b {} {}\n", common::hex(k), common::hex(v))),
            Op::ByteDel(k) => s.push_str(&format!("d {}\n", common::hex(k))),
            Op::ObjPut(oi, k, v) => {
                s.push_str(&format!("o {oi} {} {}\n", common::hex(k), common::hex(v)))
            }
        }
    }
    s
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn decode_ops(s: &str) -> Vec<Op> {
    s.lines()
        .map(|line| {
            let mut f = line.split(' ');
            match f.next().unwrap() {
                "b" => Op::BytePut(unhex(f.next().unwrap()), unhex(f.next().unwrap())),
                "d" => Op::ByteDel(unhex(f.next().unwrap())),
                "o" => Op::ObjPut(
                    f.next().unwrap().parse().unwrap(),
                    unhex(f.next().unwrap()),
                    unhex(f.next().unwrap()),
                ),
                other => panic!("bad op line: {other}"),
            }
        })
        .collect()
}

fn read_oids(d: &Path) -> [ObjectId; 3] {
    let s = std::fs::read_to_string(d.join(OIDS_FILE)).unwrap();
    let mut ids = s
        .lines()
        .map(|l| ObjectId::from_bytes(unhex(l).try_into().unwrap()));
    let oids = [
        ids.next().unwrap(),
        ids.next().unwrap(),
        ids.next().unwrap(),
    ];
    assert!(ids.next().is_none(), "more than 3 oids recorded");
    oids
}

#[test]
fn tdd029_crash_child_applies_ops() {
    if env::var_os(CHILD_ENV).is_none() {
        return; // plain suite run — the parent spawns this arm with the env
    }
    let d = PathBuf::from(env::var(DIR_ENV).unwrap());
    let ops = decode_ops(&std::fs::read_to_string(d.join(OPS_FILE)).unwrap());
    let db = open_big(&d);
    let oids = [
        db.create_object().unwrap(),
        db.create_object().unwrap(),
        db.create_object().unwrap(),
    ];
    let mut s = String::new();
    for oid in &oids {
        s.push_str(&format!("{oid}\n"));
    }
    std::fs::write(d.join(OIDS_FILE), s).unwrap();
    for op in &ops {
        apply_db(&db, op, &oids);
    }
    std::process::exit(0); // no Db::drop — the WAL stays the rows' only copy
}

proptest! {
    /// The memtable oracle: no flush, no segments — every read answers
    /// from the memtable alone.
    #[test]
    fn prop_memtable_matches_model_across_random_churn(ops in ops_strategy()) {
        let d = dir("prop-memtable");
        let (db, oids) = new_session(&d);
        let mut model = Model::default();
        for (i, op) in ops.iter().enumerate() {
            apply_db(&db, op, &oids);
            apply_model(&mut model, op);
            if i % 10 == 9 {
                check_model(&db, &model, &oids, &format!("memtable op {i}"));
            }
        }
        check_model(&db, &model, &oids, "memtable final");
        drop(db);
    }

    /// The segment oracle: flushes push the memtable into segments while
    /// churn continues — reads merge every layer. The reopen leg reads
    /// back from segments alone.
    #[test]
    fn prop_flushed_segments_match_model_across_random_churn(ops in ops_strategy()) {
        let d = dir("prop-segments");
        let (db, oids) = new_session(&d);
        let mut model = Model::default();
        for (i, op) in ops.iter().enumerate() {
            apply_db(&db, op, &oids);
            apply_model(&mut model, op);
            if i % 12 == 11 {
                db.flush().unwrap();
                check_model(&db, &model, &oids, &format!("flushed op {i}"));
            }
        }
        db.flush().unwrap();
        check_model(&db, &model, &oids, "segments final");
        drop(db); // graceful close — the reopen reads segments only
        let db = open_big(&d);
        check_model(&db, &model, &oids, "segments reopen");
    }

    /// The compaction oracle: a multi-segment set merged by compact();
    /// object rows survive the merge (present-but-filtered, never
    /// answering a byte scan) and the reopen re-reads the merged set.
    #[test]
    fn prop_compaction_matches_model_across_layers(ops in ops_strategy()) {
        let d = dir("prop-compaction");
        let (db, oids) = new_session(&d);
        let mut model = Model::default();
        let chunk = (ops.len() / 3).max(1);
        for (i, op) in ops.iter().enumerate() {
            apply_db(&db, op, &oids);
            apply_model(&mut model, op);
            if (i + 1) % chunk == 0 {
                db.flush().unwrap();
            }
        }
        db.flush().unwrap();
        let stats = db.compact().unwrap();
        assert!(
            stats.segments_in >= 2,
            "compact skipped the set ({} segments in) — the merge path went unexercised",
            stats.segments_in
        );
        check_model(&db, &model, &oids, "post-compact");
        drop(db);
        let db = open_big(&d);
        check_model(&db, &model, &oids, "post-compact reopen");
    }
}

// A second block: the WAL leg pins 96 cases (one process spawn each), so
// it carries its own config — the items-form rule takes #![proptest_config]
// only as the block's FIRST token.
proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(96))]
    /// The WAL oracle: the crash-child applies the case and exits without
    /// Db::drop — the parent's reopen replays the WAL and must equal the
    /// model exactly, object rows included (they ride the same WAL).
    #[test]
    fn prop_wal_crash_replay_matches_model(
        ops in proptest::collection::vec(op_strategy(), 16..64)
    ) {
        let d = dir("prop-wal-crash");
        std::fs::write(d.join(OPS_FILE), encode_ops(&ops)).unwrap();
        let mut child = Command::new(env::current_exe().expect("current exe"))
            .arg("--exact")
            .arg("tdd029_crash_child_applies_ops")
            .env(CHILD_ENV, "1")
            .env(DIR_ENV, &d)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash-child");
        assert!(child.wait().expect("wait crash-child").success());
        let db = open_big(&d); // recovery replays the WAL inside open
        let oids = read_oids(&d);
        let mut model = Model::default();
        for op in &ops {
            apply_model(&mut model, op);
        }
        check_model(&db, &model, &oids, "wal replay");
        drop(db);
    }
}
