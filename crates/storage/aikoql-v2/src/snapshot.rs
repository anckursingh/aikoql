//! P3-M3 — engine-native snapshot/restore (docs/IMPLEMENTATION-PLAN-PHASE3.md
//! §58–60, design §18). Manifest-based, no full decode: a snapshot pins the
//! CURRENT generation under the state READ lock (the §23 publication order —
//! segments → logs → manifest → CURRENT — makes one generation a complete,
//! self-consistent cut; the read lock keeps a concurrent checkpoint's prune
//! from deleting logs the pinned generation still needs), copies CURRENT +
//! MANIFEST-{gen} + the segments it references + the identity/replica/
//! placement logs and checkpoints ≤ gen + the torn-safe WAL prefix, verifies
//! every copied byte, then publishes the marker LAST — the marker is the
//! commit point, so a killed snapshot is never visible (restore refuses a
//! dir without one).
//!
//! Marker (bkp006 pins its bytes — python-computed before this writer
//! existed):
//!
//! `AKSN | format_version u16 LE | generation u64 LE | file_count u32 LE |
//!  per file, sorted by name: name_len u32 LE | name | file_size u64 LE |
//!  sha256-8 of the file content | sha256-8 over everything before it`

use crate::db::{manifest_path, Config, Db, WAL_FILE};
use crate::format::{checksum8, crash_park, publish_atomic_writer_staged, Cursor, FormatError};
use crate::wal::{replay_frames, Op};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MARKER_MAGIC: &[u8; 4] = b"AKSN";
const FORMAT_VERSION: u16 = 1;
/// Minimum encoded size of one marker file entry (empty name).
const MIN_FILE_ENTRY: usize = 20;

/// One copied file in the marker: its name (relative to the snapshot dir),
/// its exact byte size, and the sha256-8 integrity fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFile {
    pub name: String,
    pub size: u64,
    pub checksum: [u8; 8],
}

#[derive(Debug, Clone)]
pub struct SnapshotMarker {
    pub format_version: u16,
    pub generation: u64,
    pub files: Vec<SnapshotFile>,
}

impl SnapshotMarker {
    /// Encode with the file list sorted by name — the marker's canonical
    /// order, so the golden is unambiguous regardless of input order.
    pub fn encode(&self) -> Vec<u8> {
        let mut files: Vec<&SnapshotFile> = self.files.iter().collect();
        files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MARKER_MAGIC);
        bytes.extend_from_slice(&self.format_version.to_le_bytes());
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for f in files {
            bytes.extend_from_slice(&(f.name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(f.name.as_bytes());
            bytes.extend_from_slice(&f.size.to_le_bytes());
            bytes.extend_from_slice(&f.checksum);
        }
        bytes.extend_from_slice(&checksum8(&bytes));
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut cur = Cursor::new(bytes);
        if cur.take(4)? != MARKER_MAGIC {
            return Err(FormatError::Corrupt("snapshot marker bad magic".into()));
        }
        let format_version = cur.u16()?;
        if format_version != FORMAT_VERSION {
            return Err(FormatError::Unsupported(format!(
                "snapshot marker format version {format_version} (this build: {FORMAT_VERSION})"
            )));
        }
        let generation = cur.u64()?;
        let file_count = cur.u32()? as usize;
        if file_count > cur.remaining() / MIN_FILE_ENTRY {
            return Err(FormatError::Corrupt(format!(
                "snapshot marker file_count {file_count} cannot fit in {} remaining bytes",
                cur.remaining()
            )));
        }
        let mut files = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let name = String::from_utf8(cur.vec()?)
                .map_err(|_| FormatError::Corrupt("snapshot marker non-UTF-8 name".into()))?;
            let size = cur.u64()?;
            let checksum: [u8; 8] = cur.take(8)?.try_into().expect("sha256-8 slice");
            files.push(SnapshotFile {
                name,
                size,
                checksum,
            });
        }
        let checksum = cur.take(8)?.to_vec();
        if !cur.is_empty() {
            return Err(FormatError::Corrupt(
                "snapshot marker trailing bytes".into(),
            ));
        }
        if checksum8(&bytes[..bytes.len() - 8]) != checksum[..] {
            return Err(FormatError::Corrupt(
                "snapshot marker checksum mismatch".into(),
            ));
        }
        Ok(SnapshotMarker {
            format_version,
            generation,
            files,
        })
    }

    pub fn read(path: &Path) -> Result<Self, FormatError> {
        let bytes = std::fs::read(path).map_err(|e| {
            FormatError::Io(format!("read snapshot marker {}: {e}", path.display()))
        })?;
        Self::decode(&bytes)
    }
}

/// The snapshot dir holds exactly one marker — `SNAPSHOT-{gen:06}`.
pub fn marker_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("SNAPSHOT-{generation:06}"))
}

fn marker_generation(name: &str) -> Option<u64> {
    name.strip_prefix("SNAPSHOT-")?.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub generation: u64,
    pub file_count: u32,
    pub bytes_copied: u64,
}

/// One engine-native restore: how many rows the live db ended up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreInfo {
    pub generation: u64,
    pub rows_restored: u64,
}

/// Copy one file while hashing it — the marker needs the fingerprint, and
/// the read has to happen exactly once per file anyway.
fn copy_hashed(src: &Path, dst: &Path) -> Result<(u64, [u8; 8]), FormatError> {
    let mut input = File::open(src)
        .map_err(|e| FormatError::Io(format!("open {} for snapshot: {e}", src.display())))?;
    let mut output = File::create(dst)
        .map_err(|e| FormatError::Io(format!("create {} in snapshot: {e}", dst.display())))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| FormatError::Io(format!("read {} for snapshot: {e}", src.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output
            .write_all(&buf[..n])
            .map_err(|e| FormatError::Io(format!("write snapshot {}: {e}", dst.display())))?;
        size += n as u64;
    }
    output
        .sync_all()
        .map_err(|e| FormatError::Io(format!("sync snapshot {}: {e}", dst.display())))?;
    let full = hasher.finalize();
    Ok((size, full[..8].try_into().expect("sha256-8 slice")))
}

/// The files a generation G pins: CURRENT, MANIFEST-G, the segments the
/// manifest references, every identity/replica/placement/checkpoint log ≤ G,
/// and the WAL. LOCK and stray temp files never enter the set.
fn pinned_files(db_dir: &Path, generation: u64) -> Result<Vec<SnapshotFile>, FormatError> {
    let manifest = crate::format::Manifest::read(&manifest_path(db_dir, generation))?;
    let mut files = vec![SnapshotFile {
        name: "CURRENT".into(),
        size: 0,
        checksum: [0; 8],
    }];
    files.push(SnapshotFile {
        name: format!("MANIFEST-{generation:06}"),
        size: 0,
        checksum: [0; 8],
    });
    for rec in &manifest.segments {
        files.push(SnapshotFile {
            name: format!("SEGMENT-{:06}.seg", rec.segment_id),
            size: 0,
            checksum: [0; 8],
        });
    }
    for prefix in ["IDENTITY-", "REPLICA-", "PLACEMENT-", "CHECKPOINT-"] {
        let mut logs: Vec<SnapshotFile> = std::fs::read_dir(db_dir)
            .map_err(|e| FormatError::Io(format!("scan {}: {e}", db_dir.display())))?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let gen: u64 = name
                    .strip_prefix(prefix)?
                    .strip_suffix(".log")?
                    .parse()
                    .ok()?;
                (gen <= generation).then_some(SnapshotFile {
                    name,
                    size: 0,
                    checksum: [0; 8],
                })
            })
            .collect();
        files.append(&mut logs);
    }
    files.push(SnapshotFile {
        name: WAL_FILE.into(),
        size: 0,
        checksum: [0; 8],
    });
    Ok(files)
}

impl Db {
    /// §58 — pin the CURRENT generation and copy it out. Takes the state
    /// READ lock for the whole copy: puts block for the copy's duration
    /// (design §18 keeps writers lock-free elsewhere; a snapshot is a rare
    /// operator operation), and a concurrent checkpoint cannot prune logs
    /// the pinned generation needs.
    pub fn snapshot_to(&self, dir: &Path) -> Result<SnapshotInfo, FormatError> {
        if dir.exists() && std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) {
            return Err(FormatError::Invalid(format!(
                "snapshot dir {} is not empty",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| FormatError::Io(format!("create snapshot dir {}: {e}", dir.display())))?;

        let state = self.state.read().expect("state read lock");
        let current = crate::format::Current::read(&self.config.dir.join("CURRENT"))?;
        let generation = current.manifest_generation;

        let mut files = pinned_files(&self.config.dir, generation)?;

        // The WAL is the one mutable file in the set (GroupCommit's committer
        // appends without the state lock): read it under the wal mutex and
        // copy only the torn-safe prefix — a partial final frame never rides
        // along. The marker records that prefix size, not the file's size.
        let wal_bytes = {
            let mut wal = self.wal.lock().expect("wal mutex");
            let mut bytes = Vec::new();
            wal.seek(SeekFrom::Start(0))
                .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
            wal.read_to_end(&mut bytes)
                .map_err(|e| FormatError::Io(format!("WAL read: {e}")))?;
            bytes
        };
        let (_, wal_valid) = replay_frames(&wal_bytes)?;

        // Copy everything, hashing on the way. The WAL is special: it is
        // copied from the already-validated in-memory prefix (a committer
        // append after our locked read must never ride along), and the
        // marker records that prefix size.
        let mut bytes_copied = 0u64;
        for f in &mut files {
            let (size, checksum) = if f.name == WAL_FILE {
                std::fs::write(dir.join(&f.name), &wal_bytes[..wal_valid])
                    .map_err(|e| FormatError::Io(format!("write snapshot {}: {e}", WAL_FILE)))?;
                (wal_valid as u64, checksum8(&wal_bytes[..wal_valid]))
            } else {
                copy_hashed(&self.config.dir.join(&f.name), &dir.join(&f.name))?
            };
            f.size = size;
            f.checksum = checksum;
            bytes_copied += size;
        }
        crash_park("AIKOQL_V2_SNAP_PARK", dir, "after_copy");

        // Verify: re-read every copied byte — the marker is only published
        // over files that were just proven intact.
        for f in &files {
            let bytes = std::fs::read(dir.join(&f.name)).map_err(|e| {
                FormatError::Corrupt(format!(
                    "snapshot verify: read {}: {e}",
                    dir.join(&f.name).display()
                ))
            })?;
            if bytes.len() as u64 != f.size || checksum8(&bytes) != f.checksum {
                return Err(FormatError::Corrupt(format!(
                    "snapshot verify: {} changed after copy",
                    f.name
                )));
            }
        }
        crash_park("AIKOQL_V2_SNAP_PARK", dir, "after_verify");

        let marker = SnapshotMarker {
            format_version: FORMAT_VERSION,
            generation,
            files,
        };
        let marker_bytes = marker.encode();
        publish_atomic_writer_staged(&marker_path(dir, generation), Some("SNAPSHOT"), |w| {
            w.write_all(&marker_bytes)
        })?;
        drop(state);

        Ok(SnapshotInfo {
            generation,
            file_count: marker.files.len() as u32,
            bytes_copied,
        })
    }

    /// §60 — engine-native restore into THIS live db: verify the snapshot,
    /// materialize it in a fresh temp dir, then swap rows through this db's
    /// own write path in one frame (puts before dels — the REC-002 trait
    /// default's algorithm). Live-server-safe: the open file is never
    /// touched on disk, and rows move all-or-nothing. In-memory derived
    /// state stays stale until restart (REC-002).
    pub fn restore_from_dir(&self, src: &Path) -> Result<RestoreInfo, FormatError> {
        let generation = std::fs::read_dir(src)
            .map_err(|e| FormatError::Io(format!("scan snapshot {}: {e}", src.display())))?
            .flatten()
            .filter_map(|e| marker_generation(&e.file_name().to_string_lossy()))
            .next()
            .unwrap_or(0); // restore_from below fails closed on a missing marker
        let target = std::env::temp_dir().join(format!(
            "aikoql-restore-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let restored = restore_from(src, target.clone())?;
        let rows = restored.scan(b"")?;
        let src_keys: HashSet<Vec<u8>> = rows.iter().map(|(k, _)| k.clone()).collect();
        let mut ops: Vec<Op> = Vec::new();
        for (k, _) in self.scan(b"")? {
            if !src_keys.contains(&k) {
                ops.push(Op::Delete(k));
            }
        }
        for (k, v) in &rows {
            ops.push(Op::Put(k.clone(), v.clone()));
        }
        if !ops.is_empty() {
            self.write(&ops)?; // one frame: readers see old-or-new, never a mix
        }
        drop(restored); // release the temp dir's handles before removal
                        // ponytail: best-effort removal — a lingering temp dir under
                        // %TEMP% is swept by the test sweeper; Windows may lag one delete.
        let _ = std::fs::remove_dir_all(&target);
        Ok(RestoreInfo {
            generation,
            rows_restored: rows.len() as u64,
        })
    }
}

/// §59 — verify a snapshot dir (marker + every file it names, byte-exact)
/// and restore it into a fresh target; fail closed on any damage. The
/// snapshot dir is read-only and restorable any number of times. Returns
/// the target opened as a live db.
pub fn restore_from(src: &Path, target: PathBuf) -> Result<Db, FormatError> {
    if target.exists() && std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_some()) {
        return Err(FormatError::Invalid(format!(
            "restore target {} is not empty",
            target.display()
        )));
    }
    // Exactly one marker; a dir without one is an incomplete snapshot.
    let mut markers: Vec<(u64, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(src)
        .map_err(|e| FormatError::Io(format!("scan snapshot {}: {e}", src.display())))?
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(gen) = marker_generation(&name) {
            markers.push((gen, e.path()));
        }
    }
    if markers.is_empty() {
        return Err(FormatError::Invalid(format!(
            "{} holds no snapshot marker — incomplete snapshot",
            src.display()
        )));
    }
    if markers.len() > 1 {
        return Err(FormatError::Corrupt(format!(
            "{} holds {} snapshot markers — ambiguous",
            src.display(),
            markers.len()
        )));
    }
    let (name_gen, marker_path_buf) = markers.pop().expect("one marker");
    let marker = SnapshotMarker::read(&marker_path_buf)?;
    if marker.generation != name_gen {
        return Err(FormatError::Corrupt(format!(
            "snapshot marker file name carries generation {name_gen}, contents say {}",
            marker.generation
        )));
    }
    // Trust boundary: a marker could name arbitrary paths. Only plain file
    // names, and each must verify byte-exact before anything is copied.
    for f in &marker.files {
        if f.name
            != Path::new(&f.name)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        {
            return Err(FormatError::Corrupt(format!(
                "snapshot marker names non-plain path {:?}",
                f.name
            )));
        }
        let bytes = std::fs::read(src.join(&f.name))
            .map_err(|e| FormatError::Corrupt(format!("snapshot {} is missing: {e}", f.name)))?;
        if bytes.len() as u64 != f.size || checksum8(&bytes) != f.checksum {
            return Err(FormatError::Corrupt(format!(
                "snapshot file {} failed integrity (size/checksum)",
                f.name
            )));
        }
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| FormatError::Io(format!("create restore target {}: {e}", target.display())))?;
    for f in &marker.files {
        std::fs::copy(src.join(&f.name), target.join(&f.name))
            .map_err(|e| FormatError::Io(format!("restore {}: {e}", f.name)))?;
    }
    Db::open(Config::new(target))
}
