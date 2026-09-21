//! P3-M3 / P5-M33 — engine-native snapshot/restore
//! (docs/IMPLEMENTATION-PLAN-PHASE3.md §58–60, design §18). The crash
//! protocol is the contract: docs/snapshot-crash-protocol.md (grep-pinned
//! by snapshot_redesign.rs snp000).
//!
//! Manifest-based, no full decode: one brief state WRITE-lock window reads
//! CURRENT → G, enumerates the pinned file set (CURRENT, MANIFEST-G, the
//! segments it references, every identity/replica/placement/checkpoint log
//! ≤ G, the WAL), arms the pin (both deletion surfaces skip pinned names
//! from here on), and validates + copies the WAL's torn-safe prefix under
//! the wal mutex — the same hold as the generation read, so no flush can
//! land between them and strand rows in neither the ≤ G set nor the
//! prefix. The bulk copy then runs with NO locks: files ≤ G are immutable
//! under the publication protocol (every publish is a staged rename),
//! CURRENT is SYNTHESIZED from G (a concurrent checkpoint's rewrite can
//! never tear it away from its MANIFEST). Every copied byte is verified,
//! then the marker publishes LAST — the commit point, so a killed
//! snapshot is never visible (restore refuses a dir without one). The pin
//! disarms on every exit path (guard).
//!
//! Marker (bkp006 pins its bytes — python-computed before this writer
//! existed):
//!
//! `AKSN | format_version u16 LE | generation u64 LE | file_count u32 LE |
//!  per file, sorted by name: name_len u32 LE | name | file_size u64 LE |
//!  sha256-8 of the file content | sha256-8 over everything before it`

use crate::db::{manifest_path, Config, Db, WAL_FILE};
use crate::format::{checksum8, crash_park, publish_atomic_writer_staged, Cursor, FormatError};
use crate::wal::Op;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
        // PR6-R2-010 — the canonical form is the contract: entries are
        // unique and strictly ascending by name (encode() is the only
        // producer; decode rejects anything else instead of normalizing).
        for w in files.windows(2) {
            if w[0].name >= w[1].name {
                return Err(FormatError::Corrupt(format!(
                    "snapshot marker entries not strictly ascending ({} then {})",
                    w[0].name, w[1].name
                )));
            }
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

/// PR6-R2-001 — the marker's structural contract: exactly the files that
/// make one generation self-consistent, no more (uniqueness) and no less.
/// Runs before the target is materialized; the checksum is NOT part of
/// this trust decision (checksum8 is not tamper resistance — the marker
/// must stand on its structure).
fn validate_snapshot_manifest(marker: &SnapshotMarker, src: &Path) -> Result<(), FormatError> {
    let mut seen = HashSet::with_capacity(marker.files.len());
    for f in &marker.files {
        if !seen.insert(f.name.as_str()) {
            return Err(FormatError::Corrupt(format!(
                "snapshot marker names {} twice",
                f.name
            )));
        }
    }
    let has = |name: &str| seen.contains(name);
    if !has("CURRENT") {
        return Err(FormatError::Corrupt(
            "snapshot marker carries no CURRENT".into(),
        ));
    }
    let manifest_name = format!("MANIFEST-{:06}", marker.generation);
    if !has(&manifest_name) {
        return Err(FormatError::Corrupt(format!(
            "snapshot marker carries no {manifest_name}"
        )));
    }
    if !has(WAL_FILE) {
        return Err(FormatError::Corrupt(format!(
            "snapshot marker carries no {WAL_FILE}"
        )));
    }
    let manifest = crate::format::Manifest::read(&src.join(&manifest_name))?;
    for rec in &manifest.segments {
        let seg = format!("SEGMENT-{:06}.seg", rec.segment_id);
        if !has(&seg) {
            return Err(FormatError::Corrupt(format!(
                "snapshot marker omits {seg} referenced by {manifest_name}"
            )));
        }
    }
    // The manifest publishes per-family floors: each non-zero floor's log
    // must ride along (the delta relationship the manifest claims).
    for (floor, prefix) in [
        (manifest.identity_floor, "IDENTITY-"),
        (manifest.replica_floor, "REPLICA-"),
        (manifest.placement_floor, "PLACEMENT-"),
    ] {
        if floor > 0 && !has(&format!("{prefix}{floor:06}.log")) {
            return Err(FormatError::Corrupt(format!(
                "snapshot marker omits {prefix}{floor:06}.log — manifest floor {floor}"
            )));
        }
    }
    Ok(())
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

/// The snapshot's CURRENT is synthesized from the pinned generation, never
/// re-read from the live file — a concurrent checkpoint rewrites CURRENT,
/// and the synthesized 22 bytes can never tear away from MANIFEST-G (the
/// protocol doc's "mix impossible by construction"). Hashed like any other
/// copied file, so the marker and the verify pass treat it uniformly.
fn write_current(dst: &Path, generation: u64) -> Result<(u64, [u8; 8]), FormatError> {
    let bytes = crate::format::Current::new(FORMAT_VERSION, generation).encode();
    let mut out = File::create(dst)
        .map_err(|e| FormatError::Io(format!("create {} in snapshot: {e}", dst.display())))?;
    out.write_all(&bytes)
        .map_err(|e| FormatError::Io(format!("write snapshot CURRENT: {e}")))?;
    out.sync_all()
        .map_err(|e| FormatError::Io(format!("sync snapshot CURRENT: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let full = hasher.finalize();
    Ok((
        bytes.len() as u64,
        full[..8].try_into().expect("sha256-8 slice"),
    ))
}

/// Stream a file while hashing it — restore verification must never hold
/// the largest snapshot file in memory (PR6-R2-011). Same primitive as
/// `copy_hashed`, without the destination.
fn hash_streamed(path: &Path) -> Result<(u64, [u8; 8]), FormatError> {
    let mut input = File::open(path)
        .map_err(|e| FormatError::Io(format!("open {} for restore: {e}", path.display())))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| FormatError::Io(format!("read {} for restore: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    let full = hasher.finalize();
    Ok((size, full[..8].try_into().expect("sha256-8 slice")))
}

/// Validate the WAL (bounded memory) and copy the torn-safe prefix to
/// `dst`, hashing on the way — PR6-R3-003: snapshot creation must not
/// materialize the WAL, so the validation is streamed (`valid_prefix_len`
/// holds one frame at a time) and the copy is a fixed 64 KiB buffer. The
/// caller holds the wal mutex for the whole span, so the file cannot
/// change between the two passes; the marker records the prefix size.
fn copy_wal_prefix(wal: &mut File, dst: &Path) -> Result<(u64, [u8; 8]), FormatError> {
    wal.seek(SeekFrom::Start(0))
        .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
    let wal_valid = crate::wal::valid_prefix_len(wal)?;
    wal.seek(SeekFrom::Start(0))
        .map_err(|e| FormatError::Io(format!("WAL seek: {e}")))?;
    let mut out = File::create(dst)
        .map_err(|e| FormatError::Io(format!("write snapshot {}: {e}", dst.display())))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut left = wal_valid;
    while left > 0 {
        let want = (buf.len() as u64).min(left) as usize;
        let n = wal
            .read(&mut buf[..want])
            .map_err(|e| FormatError::Io(format!("WAL read: {e}")))?;
        if n == 0 {
            return Err(FormatError::Io("WAL shrank during snapshot copy".into()));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| FormatError::Io(format!("write snapshot {}: {e}", dst.display())))?;
        left -= n as u64;
    }
    out.sync_all()
        .map_err(|e| FormatError::Io(format!("sync snapshot {}: {e}", dst.display())))?;
    let full = hasher.finalize();
    Ok((wal_valid, full[..8].try_into().expect("sha256-8 slice")))
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

/// P5-M33 — the pin disarm: both deletion surfaces consult
/// `State::snapshot_pins`; the guard removes the captured names on every
/// exit path (success or error) so a pin can never leak into a slow disk
/// leak. Poison-recovering — a panic elsewhere must not leave pins armed.
struct SnapshotPinGuard<'a> {
    db: &'a Db,
    names: Vec<String>,
}

impl Drop for SnapshotPinGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.db.state.write().unwrap_or_else(|e| e.into_inner());
        for name in &self.names {
            state.snapshot_pins.remove(name);
        }
    }
}

/// P5-M34 — env-gated measurement cells (`AIKOQL_V2_SNAP_CELLS=path`):
/// phase walls + byte counts for the double-read report (snp004 pins the
/// sidecar's shape). `new(None)` is inert — every method no-ops on the
/// Option (the read-trace 0=off pattern). The sidecar is diagnostic: a
/// write failure warns and never fails the snapshot.
struct SnapCells {
    path: Option<PathBuf>,
    started: Instant,
    spans: Vec<(String, u64)>,
}

impl SnapCells {
    fn new(path: Option<std::ffi::OsString>) -> Self {
        SnapCells {
            path: path.map(PathBuf::from),
            started: Instant::now(),
            spans: Vec::new(),
        }
    }

    /// Close the previous phase (if any) and open `name`.
    fn phase(&mut self, name: &str) {
        if self.path.is_none() {
            return;
        }
        self.spans
            .push((name.to_string(), self.started.elapsed().as_millis() as u64));
        self.started = Instant::now();
    }

    /// Close the last phase and write the sidecar. `read_bytes` = 2 ×
    /// copied — the copy read and the verify re-read (the double-read
    /// claim, stated as derived).
    fn finish(&mut self, bytes_copied: u64, file_count: u32) {
        let Some(path) = &self.path else { return };
        self.spans.push((
            "marker_ms".into(),
            self.started.elapsed().as_millis() as u64,
        ));
        let mut body = String::from("{");
        for (k, v) in &self.spans {
            body.push_str(&format!("\"{k}\":{v},"));
        }
        body.push_str(&format!(
            "\"bytes_copied\":{bytes_copied},\"read_bytes\":{},\"file_count\":{file_count}}}",
            bytes_copied * 2
        ));
        if let Err(e) = std::fs::write(path, &body) {
            eprintln!("aikoql-v2: snapshot cells not written: {e}");
        }
    }
}

impl Db {
    /// P5-M33 (§58) — the protocol in docs/snapshot-crash-protocol.md.
    /// Capture + arm in one state WRITE hold: generation read, file-set
    /// enumeration, pin inserts, and the WAL validate + copy (wal mutex,
    /// state → wal order — commit_group's). The hold is the snapshot's
    /// whole writer-blocking span. The bulk copy + verify + marker then
    /// run with no locks held; the pin disarms via the guard on every
    /// exit path.
    pub fn snapshot_to(&self, dir: &Path) -> Result<SnapshotInfo, FormatError> {
        if dir.exists() && std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) {
            return Err(FormatError::Invalid(format!(
                "snapshot dir {} is not empty",
                dir.display()
            )));
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| FormatError::Io(format!("create snapshot dir {}: {e}", dir.display())))?;
        let mut cells = SnapCells::new(std::env::var_os("AIKOQL_V2_SNAP_CELLS"));

        // Capture + arm — the one locked window. The pin inserts come
        // last, so no error path can leak a pin. The WAL copy rides the
        // SAME hold as the generation read: a flush between the two would
        // strand its rows in neither the ≤ G file set nor the captured
        // prefix.
        let (generation, mut files, wal_size, wal_checksum) = {
            let mut state = self.state.write().expect("state write lock");
            let current = crate::format::Current::read(&self.config.dir.join("CURRENT"))?;
            let generation = current.manifest_generation;
            let files = pinned_files(&self.config.dir, generation)?;
            // The WAL is the one mutable file in the set (GroupCommit's
            // committer appends, every flush truncates): validate + copy
            // inside one wal-mutex hold, torn-safe prefix only, streamed
            // — bounded memory, PR6-R3-003. The marker records that
            // prefix size, not the file's.
            let mut wal = self.wal.lock().expect("wal mutex");
            let (wal_size, wal_checksum) = copy_wal_prefix(&mut wal, &dir.join(WAL_FILE))?;
            for f in &files {
                state.snapshot_pins.insert(f.name.clone());
            }
            (generation, files, wal_size, wal_checksum)
        };
        // Disarm on every exit path from here on — success or error (the
        // protocol doc: a leaked pin is a slow disk leak). Poison-
        // recovering: a panic elsewhere must not leave the pins armed.
        let _pin = SnapshotPinGuard {
            db: self,
            names: files.iter().map(|f| f.name.clone()).collect(),
        };
        cells.phase("capture_ms");

        // Bulk copy, hashing on the way — no locks held. CURRENT is
        // synthesized from the pinned generation; everything else is
        // immutable under the publication protocol and protected from the
        // prune surfaces by the pin.
        let mut bytes_copied = wal_size;
        for (i, f) in files.iter_mut().enumerate() {
            if f.name == WAL_FILE {
                f.size = wal_size;
                f.checksum = wal_checksum;
                continue; // copied above, under the wal mutex
            }
            // PR6-007 — CURRENT is already copied: a kill here leaves an
            // unmarked dir (row 5); deleting the marker file releases the
            // park for the interleave rows — write/flush/checkpoint/
            // compaction issued while parked land CONCURRENTLY with this
            // lock-free copy, and the pin keeps the captured set intact
            // (rows 1–4, the M33 redesign).
            if i == 1 {
                crash_park("AIKOQL_V2_SNAP_PARK", dir, "during_copy");
            }
            let (size, checksum) = if f.name == "CURRENT" {
                write_current(&dir.join(&f.name), generation)?
            } else {
                copy_hashed(&self.config.dir.join(&f.name), &dir.join(&f.name))?
            };
            f.size = size;
            f.checksum = checksum;
            bytes_copied += size;
        }
        crash_park("AIKOQL_V2_SNAP_PARK", dir, "after_copy");
        cells.phase("copy_ms");

        // Verify: re-read every copied byte — the marker is only published
        // over files that were just proven intact. PR6-R2-011 — streamed,
        // so the verify never holds a whole file in memory.
        for f in &files {
            let (size, sum) = hash_streamed(&dir.join(&f.name)).map_err(|e| {
                FormatError::Corrupt(format!(
                    "snapshot verify: read {}: {e}",
                    dir.join(&f.name).display()
                ))
            })?;
            if size != f.size || sum != f.checksum {
                return Err(FormatError::Corrupt(format!(
                    "snapshot verify: {} changed after copy",
                    f.name
                )));
            }
        }
        crash_park("AIKOQL_V2_SNAP_PARK", dir, "after_verify");
        cells.phase("verify_ms");

        let marker = SnapshotMarker {
            format_version: FORMAT_VERSION,
            generation,
            files,
        };
        let marker_bytes = marker.encode();
        publish_atomic_writer_staged(&marker_path(dir, generation), Some("SNAPSHOT"), |w| {
            w.write_all(&marker_bytes)
        })?;
        // PR6-007 — the marker is fully committed; a kill here must leave a
        // restorable snapshot (row 6).
        crash_park("AIKOQL_V2_SNAP_PARK", dir, "after_marker");
        cells.finish(bytes_copied, marker.files.len() as u32);

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
    // PR6-R2-001 — the structural contract comes before any file IO on the
    // target: a marker naming valid-but-incomplete files is rejected here,
    // never half-materialized then failed by Db::open.
    validate_snapshot_manifest(&marker, src)?;
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
        // PR6-R2-011 — streamed verify: peak restore memory is a fixed
        // 64 KiB buffer, never the largest snapshot file.
        let (size, sum) = hash_streamed(&src.join(&f.name))
            .map_err(|e| FormatError::Corrupt(format!("snapshot {} is missing: {e}", f.name)))?;
        if size != f.size || sum != f.checksum {
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
