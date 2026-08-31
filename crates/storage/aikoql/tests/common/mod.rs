//! Shared harness pieces for the KSE measurement suites (kse5, kse6, …).
#![allow(dead_code)] // each suite uses only the pieces it needs

use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use aikoql_kernel::{Direction, Kernel, KnowledgeContext, Subject, KOID};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Pass-through engine that counts every kernel→engine request.
pub struct CountingEngine {
    pub inner: Arc<dyn StorageEngine>,
    gets: AtomicU64,
    scan_calls: AtomicU64,
    scan_pairs: AtomicU64,
    bytes_returned: AtomicU64,
    write_batches: AtomicU64,
    puts: AtomicU64,
    dels: AtomicU64,
}

impl CountingEngine {
    pub fn new(inner: Arc<dyn StorageEngine>) -> Arc<Self> {
        Arc::new(CountingEngine {
            inner,
            gets: AtomicU64::new(0),
            scan_calls: AtomicU64::new(0),
            scan_pairs: AtomicU64::new(0),
            bytes_returned: AtomicU64::new(0),
            write_batches: AtomicU64::new(0),
            puts: AtomicU64::new(0),
            dels: AtomicU64::new(0),
        })
    }
}

impl StorageEngine for CountingEngine {
    fn get(&self, key: &[u8]) -> aikoql_kernel::KResult<Option<Vec<u8>>> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        let v = self.inner.get(key)?;
        if let Some(v) = &v {
            self.bytes_returned
                .fetch_add(v.len() as u64, Ordering::Relaxed);
        }
        Ok(v)
    }

    fn scan(&self, prefix: &[u8]) -> aikoql_kernel::KResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_calls.fetch_add(1, Ordering::Relaxed);
        let rows = self.inner.scan(prefix)?;
        self.scan_pairs
            .fetch_add(rows.len() as u64, Ordering::Relaxed);
        let bytes: u64 = rows.iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
        self.bytes_returned.fetch_add(bytes, Ordering::Relaxed);
        Ok(rows)
    }

    fn write_batch(&self, batch: &WriteBatch) -> aikoql_kernel::KResult<()> {
        self.write_batches.fetch_add(1, Ordering::Relaxed);
        self.puts
            .fetch_add(batch.puts.len() as u64, Ordering::Relaxed);
        self.dels
            .fetch_add(batch.dels.len() as u64, Ordering::Relaxed);
        self.inner.write_batch(batch)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalCounts {
    pub gets: u64,
    pub scans: u64,
    pub pairs: u64,
    pub bytes: u64,
}

impl LogicalCounts {
    pub fn snapshot(c: &CountingEngine) -> LogicalCounts {
        LogicalCounts {
            gets: c.gets.load(Ordering::Relaxed),
            scans: c.scan_calls.load(Ordering::Relaxed),
            pairs: c.scan_pairs.load(Ordering::Relaxed),
            bytes: c.bytes_returned.load(Ordering::Relaxed),
        }
    }

    pub fn delta(&self, before: LogicalCounts) -> LogicalCounts {
        LogicalCounts {
            gets: self.gets - before.gets,
            scans: self.scans - before.scans,
            pairs: self.pairs - before.pairs,
            bytes: self.bytes - before.bytes,
        }
    }

    pub fn writes(c: &CountingEngine) -> (u64, u64, u64) {
        (
            c.write_batches.load(Ordering::Relaxed),
            c.puts.load(Ordering::Relaxed),
            c.dels.load(Ordering::Relaxed),
        )
    }
}

impl std::fmt::Display for LogicalCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} gets + {} scans ({} pairs, {} B returned)",
            self.gets, self.scans, self.pairs, self.bytes
        )
    }
}

pub fn percentiles(mut xs: Vec<u128>) -> (u128, u128, u128) {
    xs.sort_unstable();
    let p = |q: f64| xs[((xs.len() - 1) as f64 * q).round() as usize];
    (p(0.50), p(0.95), p(0.99))
}

pub fn tmp(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("aikoql_kse_unit_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_dir_all(&p);
    p
}

// ---------------------------------------------------------------------------
// Model-free structural sweep (kse14, kse15): every invariant that must hold
// at ANY batch boundary, computed from the store's own rows — no reference
// model, so it applies wherever the capture/crash point is unknown.
// ---------------------------------------------------------------------------

pub fn ctx() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<String>()
}

fn derived_keys(engine: &dyn StorageEngine) -> BTreeSet<Vec<u8>> {
    [
        b"relo/".as_slice(),
        b"reli/".as_slice(),
        b"type/".as_slice(),
    ]
    .into_iter()
    .flat_map(|p| engine.scan(p).unwrap())
    .map(|(k, _v)| k)
    .collect()
}

pub fn structural_sweep(k: &Kernel, engine: &dyn StorageEngine, label: &str) {
    let heads: Vec<Vec<u8>> = engine
        .scan(b"head/")
        .unwrap()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(
        heads.len(),
        heads.iter().collect::<BTreeSet<_>>().len(),
        "{label}: duplicate KOID in head/"
    );
    let head_koids: BTreeSet<Vec<u8>> = heads.iter().map(|key| key[5..].to_vec()).collect();

    // Version rows: exactly one per (koid, ts); every row's KOID has a head.
    let mut version_rows = BTreeSet::new();
    for (key, _v) in engine.scan(b"ko/").unwrap() {
        assert_eq!(key.len(), 3 + 16 + 8, "{label}: malformed version key");
        assert!(
            head_koids.contains(&key[3..19]),
            "{label}: version row {} for a KOID with no head",
            hex(&key)
        );
        assert!(
            version_rows.insert(key.clone()),
            "{label}: duplicate (koid, ts) row {}",
            hex(&key)
        );
    }

    // One journal event per version (QA2-PROP invariant), seqs exactly 1..=n.
    let seqs: Vec<u64> = engine
        .scan(b"ke/")
        .unwrap()
        .into_iter()
        .map(|(key, _)| {
            assert_eq!(key.len(), 3 + 8, "{label}: malformed event key");
            u64::from_be_bytes(key[3..].try_into().unwrap())
        })
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "{label}: journal seqs not exactly 1..=n"
    );
    assert_eq!(
        seqs.len(),
        version_rows.len(),
        "{label}: journal events != version rows"
    );

    // Every head: coherent provenance, contiguous lineage, sane interval —
    // and the derived-set image is computed FROM these heads below.
    let mut image = BTreeSet::new();
    for key in &heads {
        let koid = KOID::from_hex(&hex(&key[5..])).unwrap();
        let head = k
            .get(ctx(), &koid)
            .unwrap_or_else(|e| panic!("{label}: get head {} failed: {e:?}", koid.to_hex()));
        assert_eq!(
            head.event_refs.len(),
            head.version as usize,
            "{label}: half-committed head {}",
            koid.to_hex()
        );
        assert!(
            head.event_refs.windows(2).all(|w| w[0].seq < w[1].seq),
            "{label}: event seqs not increasing on {}",
            koid.to_hex()
        );
        if let (Some(f), Some(t)) = (head.valid_from(), head.valid_to()) {
            assert!(f <= t, "{label}: inverted interval on {}", koid.to_hex());
        }
        let tr = k.trace(ctx(), &koid).unwrap();
        assert_eq!(
            tr.versions.len(),
            head.version as usize,
            "{label}: lineage length != version on {}",
            koid.to_hex()
        );
        assert!(
            tr.versions
                .windows(2)
                .all(|w| w[0].version + 1 == w[1].version),
            "{label}: gapped lineage on {}",
            koid.to_hex()
        );
        assert!(
            tr.versions
                .windows(2)
                .all(|w| w[0].commit_ts <= w[1].commit_ts),
            "{label}: commit_ts ran backwards on {}",
            koid.to_hex()
        );
        for r in &head.relationships {
            let (src, dst) = match r.direction {
                Direction::Outbound => (&koid, &r.target),
                Direction::Inbound => (&r.target, &koid),
            };
            let mut relo = b"relo/".to_vec();
            relo.extend_from_slice(src.as_bytes());
            relo.push(b'/');
            relo.extend_from_slice(r.rel_type.as_bytes());
            relo.push(b'/');
            relo.extend_from_slice(dst.as_bytes());
            image.insert(relo);
            let mut reli = b"reli/".to_vec();
            reli.extend_from_slice(dst.as_bytes());
            reli.push(b'/');
            reli.extend_from_slice(r.rel_type.as_bytes());
            reli.push(b'/');
            reli.extend_from_slice(src.as_bytes());
            image.insert(reli);
        }
        let mut tk = b"type/".to_vec();
        tk.extend_from_slice(head.metadata.type_name.as_bytes());
        tk.push(b'/');
        tk.extend_from_slice(koid.as_bytes());
        image.insert(tk);
    }
    assert_eq!(
        derived_keys(engine),
        image,
        "{label}: derived indexes drifted from their own heads"
    );
    let report = k.rebuild_derived_indexes().unwrap();
    assert_eq!(
        (report.removed_stale, report.removed_invalid),
        (0, 0),
        "{label}: rebuild found drift the sweep missed"
    );
    assert_eq!(
        derived_keys(engine),
        image,
        "{label}: rebuild changed the derived set"
    );
}
