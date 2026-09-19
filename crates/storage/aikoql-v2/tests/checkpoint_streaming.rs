//! P4-M6 — the streaming checkpoint writer (TDD-ID-003): the checkpoint is
//! published as streamed records into the staged temp (no full encoded
//! buffer for large datasets), byte-identical to the materialized form.
//! REDs: cps001 streamed == materialized byte-for-byte; cps002 crash during
//! the streamed publish leaves the old checkpoint authoritative; cps003
//! (env-gated, CPS_NIGHTLY=1) the 1M peak-RSS cell; cps004 the
//! deterministic allocation proof behind that cell (PR6-R2-006).

mod common;

use aikoql_storage_v2::checkpoint::{
    checkpoint_generation, checkpoint_path, test_support, DirectoryCheckpoint,
};
use aikoql_storage_v2::db::{Config, Db};
use aikoql_storage_v2::identity::{LogicalId, ObjectId, ReplicaId};
use aikoql_storage_v2::placement::directory::{PhysicalLocation, Placement};
use aikoql_storage_v2::placement::{BlockId, SegmentId};
use common::dir;
use std::collections::HashMap;
use std::io::BufRead as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn oid(b: u8) -> ObjectId {
    ObjectId([b; 16])
}

/// One of each placement variant plus ordinary records — the encode/decode
/// matrix, built as the Db builds it (from_state sorts by key).
fn sample_checkpoint() -> DirectoryCheckpoint {
    let mut identity = HashMap::new();
    let mut replicas = HashMap::new();
    let mut placements = HashMap::new();
    for i in 0..24u64 {
        let o = ObjectId([i as u8; 16]);
        let lid = LogicalId(i + 1);
        let rid = ReplicaId(100 + i);
        identity.insert(o, lid);
        replicas.insert(lid, rid);
        placements.insert(
            rid,
            match i % 3 {
                0 => Placement::Memtable { generation: i },
                1 => Placement::Segment(PhysicalLocation {
                    segment_id: SegmentId(i * 7 + 1),
                    block_id: BlockId(i as u32 + 2),
                    entry_offset: i as u32 * 3,
                    generation: i * 11,
                }),
                _ => Placement::Retired { generation: i },
            },
        );
    }
    DirectoryCheckpoint::from_state(42, &identity, &replicas, &placements, 25, 124, 254)
}

// ---------------------------------------------------------------------------
// cps001 — streamed publish == materialized publish, byte for byte
// ---------------------------------------------------------------------------

#[test]
fn cps001_streamed_equals_materialized_byte_for_byte() {
    let cp = sample_checkpoint();
    let d = dir("cps001");
    let strm_path = d.join("streamed.log");

    DirectoryCheckpoint::publish_staged_streamed(&strm_path, &cp, None).unwrap();

    let strm = std::fs::read(&strm_path).unwrap();
    assert_eq!(
        strm,
        test_support::encode_for_tests(&cp),
        "streamed publish is byte-identical to the materialized reference form"
    );
    assert_eq!(
        DirectoryCheckpoint::read(&strm_path).unwrap(),
        cp,
        "the streamed file decodes to the same checkpoint"
    );
}

// ---------------------------------------------------------------------------
// cps002 — crash during the streamed publish: the OLD checkpoint stays
// authoritative and the torn temp never poisons recovery
// ---------------------------------------------------------------------------

const CHILD_ENV: &str = "CPS002_CHILD";
const DIR_ENV: &str = "CPS002_DIR";

fn child_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var(DIR_ENV).expect("child dir env"))
}

fn spawn_cps_child(test_name: &str, d: &Path) -> Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .arg("--exact")
        .arg(test_name)
        .env(CHILD_ENV, "1")
        .env(DIR_ENV, d)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child")
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < timeout,
            "marker {} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn checkpoint_gens(d: &Path) -> Vec<u64> {
    let mut gens: Vec<u64> = std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .filter_map(|e| checkpoint_generation(&e.file_name().to_string_lossy()))
        .collect();
    gens.sort_unstable();
    gens
}

fn cps_workload(db: &Db, base: u8) {
    for b in base..base + 30 {
        let a = oid(b);
        db.put_object(a, b"k1", &[b, 1]).unwrap();
        db.put_object(a, b"k2", &[b, 2]).unwrap();
    }
}

fn verify_all(db: &Db) {
    for b in 0x01u8..=0x3C {
        let a = oid(b);
        assert_eq!(db.get_object(a, b"k1").unwrap(), Some(vec![b, 1]));
        assert_eq!(db.get_object(a, b"k2").unwrap(), Some(vec![b, 2]));
    }
}

#[test]
fn cps002_crash_during_streamed_publish_keeps_old_checkpoint() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let mut cfg = Config::new(child_dir());
        cfg.checkpoint_bytes = 256; // first flush crosses the trigger
        let db = Db::open(cfg).unwrap();
        cps_workload(&db, 0x01);
        db.flush().unwrap(); // publishes the FIRST checkpoint (gen 2, ckp004 pin)
                             // Arm the park only now: the scenario is a torn SECOND publish.
        std::env::set_var("AIKOQL_V2_PLACE_PARK", "FAIL_AFTER_CHECKPOINT_WRITE");
        cps_workload(&db, 0x1F);
        db.flush().unwrap(); // parks inside the SECOND checkpoint's streamed write
        unreachable!("the parent kills the parked child");
    }
    let d = dir("cps002");
    let mut child = spawn_cps_child(
        "cps002_crash_during_streamed_publish_keeps_old_checkpoint",
        &d,
    );
    wait_for(
        &d.join("FAIL_AFTER_CHECKPOINT_WRITE"),
        Duration::from_secs(60),
    );
    child.kill().expect("kill child");
    child.wait().expect("wait child");

    assert_eq!(checkpoint_gens(&d), vec![2], "the old checkpoint survived");
    let torn: Vec<String> = std::fs::read_dir(&d)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(".CHECKPOINT-").then_some(n)
        })
        .collect();
    assert_eq!(torn.len(), 1, "the torn streamed temp is still on disk");
    // Reopen WITH the torn temp present: it must be ignored, the old
    // checkpoint + surviving deltas converge to the full state.
    let db = Db::open(Config::new(d.clone())).unwrap();
    verify_all(&db);
}

// ---------------------------------------------------------------------------
// cps003 — env-gated (CPS_NIGHTLY=1) 1M peak-RSS cell: holding the
// materialized encode buffer must raise peak RSS well above the streamed
// publish's peak (which never allocates that buffer)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn self_peak_over(pid: u32, ms: u32) -> u64 {
    let script = format!(
        "while (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Write-Output (Get-Process -Id {pid}).WorkingSet64; Start-Sleep -Milliseconds 100 }}"
    );
    let mut sampler = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = sampler.stdout.take().unwrap();
    let mut peak = 0u64;
    for line in std::io::BufReader::new(out)
        .lines()
        .map_while(Result::ok)
        .take(ms as usize / 100 + 1)
    {
        if let Ok(v) = line.trim().parse::<u64>() {
            peak = peak.max(v);
        }
    }
    // Self-sampling: PowerShell exits when THIS process exits, so waiting
    // here deadlocks (its pipe fills, it blocks, we wait forever). Kill it.
    let _ = sampler.kill();
    let _ = sampler.wait();
    peak
}

#[test]
fn cps003_peak_rss_streamed_not_above_materialized() {
    if std::env::var_os("CPS_NIGHTLY").is_none() {
        return; // strict opt-in: builds a 1M-object directory state
    }
    #[cfg(not(windows))]
    {
        return; // WorkingSet64 polling is Windows-only (the mem001 precedent)
    }
    #[cfg(windows)]
    {
        let d = dir("cps003");
        let n: u64 = 1_000_000;
        let mut identity = HashMap::new();
        let mut replicas = HashMap::new();
        let mut placements = HashMap::new();
        for i in 0..n {
            let o = ObjectId(i.to_le_bytes().repeat(2).try_into().unwrap());
            let lid = LogicalId(i + 1);
            let rid = ReplicaId(i + 1);
            identity.insert(o, lid);
            replicas.insert(lid, rid);
            placements.insert(rid, Placement::Memtable { generation: 1 });
        }
        let cp =
            DirectoryCheckpoint::from_state(1, &identity, &replicas, &placements, n + 1, n + 1, 2);
        let held = test_support::encode_for_tests(&cp); // the materialized path's transient buffer
        let encode_len = held.len() as u64;
        let mat_peak = self_peak_over(std::process::id(), 2500);
        drop(held);
        let path = checkpoint_path(&d, 1);
        DirectoryCheckpoint::publish_staged_streamed(&path, &cp, None).unwrap();
        let strm_peak = self_peak_over(std::process::id(), 2500);
        println!(
            "CPS003 report (n={n}, encode {encode_len} B): materialized peak {mat_peak} B, \
             streamed peak {strm_peak} B"
        );
        assert!(
            strm_peak + encode_len / 2 <= mat_peak,
            "the streamed publish must not allocate the encoded buffer: \
             streamed {strm_peak} + half-buffer {} > materialized {mat_peak}",
            encode_len / 2
        );
    }
}

// ---------------------------------------------------------------------------
// cps004 — PR6-R2-006: the deterministic allocation proof behind the cps003
// RSS cell. Every write_all slice write_streamed() issues is measured; the
// largest must stay at record scale (≤ 33 B, the placement record), never
// one buffer proportional to the checkpoint image — the thing the RSS cell
// can only observe empirically.
// ---------------------------------------------------------------------------

#[test]
fn cps004_streamed_writes_are_record_sized_not_image_sized() {
    struct MaxCallWriter {
        bytes: usize,
        max_call: usize,
    }
    impl std::io::Write for MaxCallWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes += buf.len();
            self.max_call = self.max_call.max(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // 100k of each record: the encoded image is ~8.1 MB — an image-sized
    // write would dwarf the record bound by five orders of magnitude.
    let n: u64 = 100_000;
    let mut identity = HashMap::new();
    let mut replicas = HashMap::new();
    let mut placements = HashMap::new();
    for i in 0..n {
        let o = ObjectId(i.to_le_bytes().repeat(2).try_into().unwrap());
        let lid = LogicalId(i + 1);
        let rid = ReplicaId(i + 1);
        identity.insert(o, lid);
        replicas.insert(lid, rid);
        placements.insert(rid, Placement::Memtable { generation: 1 });
    }
    let cp = DirectoryCheckpoint::from_state(1, &identity, &replicas, &placements, n + 1, n + 1, 2);

    let mut w = MaxCallWriter {
        bytes: 0,
        max_call: 0,
    };
    cp.write_streamed(&mut w).unwrap();

    // magic 4 + version 2 + generation 8 + 3×count 4 + identity 24n +
    // replica 24n + memtable placement 33n + floors 24 + checksum 8.
    let expect: usize =
        4 + 2 + 8 + 12 + 24 * n as usize + 24 * n as usize + 33 * n as usize + 24 + 8;
    assert_eq!(w.bytes, expect, "the streamed byte count is exact");
    assert!(
        w.max_call <= 33,
        "the largest single write is {} B — record scale, not the {} B image",
        w.max_call,
        w.bytes
    );
}
