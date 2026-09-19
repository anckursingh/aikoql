//! SE2-M40 — the directory checkpoint (review P0-1/M5-M7): one coordinated
//! snapshot of the identity + replica + placement directories at one
//! manifest generation, so recovery is
//!
//! ```text
//! newest valid checkpoint ≤ CURRENT
//! + only delta logs published after that checkpoint
//! + the active WAL
//! ```
//!
//! instead of replaying the full metadata history since database creation.
//! The three directories are ONE consistency domain (a flush publishes all
//! three at the same generation), so ONE file with ONE atomic publish
//! carries all of them — per-directory checkpoints would need a joint
//! marker to be atomic (review Challenge B: coordinated wins).
//!
//! On-disk shape (all little-endian; the crate's sha256-8 checksum):
//! `AKCK | format_version u16 | generation u64 |
//!  identity_count u32 | records (ObjectId 16 | LogicalId 8) |
//!  replica_count u32 | records (LogicalId 8 | NodeId 8 | ReplicaId 8) |
//!  placement_count u32 | records (the placement record shape: ReplicaId 8 |
//!  variant u8 | SegmentId 8 | BlockId 4 | entry_offset u32 | generation u64) |
//!  checksum8`
//! Files are named `CHECKPOINT-{gen:06}.log` beside the manifest.
//!
//! The write protocol (review P0-2): publish (atomic write-temp → fsync →
//! rename) → VERIFY (read back + decode — a checkpoint is only trusted to
//! prune history after it proves decodable) → prune delta logs at or below
//! its generation → drop older checkpoints. The checkpoint publishes AFTER
//! CURRENT already names its generation, so it adds NO new crash state:
//! before the publish the full delta history is still there; after it the
//! checkpoint covers ≤ G and the deltas above G replay on top. Pruning a
//! leftover log is idempotent-safe anyway (the merge gates re-apply
//! duplicates as no-ops — the invariants that make crash windows converge).
//!
//! Damage policy: a corrupt checkpoint fails closed ALWAYS (no fallback to
//! the deltas). The verify-at-write step shrinks the window where a
//! checkpoint could be damaged-but-unpruned to milliseconds, and a partial
//! prune makes "fall back to the surviving deltas" silently unsound — the
//! surviving logs may not cover the pruned range. Fail closed preserves the
//! operator's evidence (the v1 closure's Q1 policy, verbatim).

use crate::format::{
    chain_extend, checksum8, crash_park, publish_atomic_writer_staged, Cursor, FormatError,
    Manifest, FORMAT_VERSION,
};
use crate::identity::directory::{
    identity_log_generation, identity_log_path, replica_log_path, IdentityLog, IdentityRecord,
    ReplicaLog, ReplicaRecord,
};
use crate::identity::{NodeId, LOCAL_NODE_ID};
use crate::placement::directory::{placement_log_path, PlacementLog, PlacementRecord};
use crate::placement::{BlockId, Placement, SegmentId};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::mem::size_of;
use std::path::{Path, PathBuf};

const CHECKPOINT_MAGIC: &[u8; 4] = b"AKCK";
/// oid 16 + lid 8 — the identity log's record shape, verbatim.
const IDENTITY_RECORD_LEN: usize = 24;
/// lid 8 + node 8 + rid 8 — the replica log's record shape.
const REPLICA_RECORD_LEN: usize = 24;
/// rid 8 + variant 1 + segment 8 + block 4 + entry 4 + generation 8.
const PLACEMENT_RECORD_LEN: usize = 33;
/// PR6-R2-009 — computed, so the constant can never drift from the
/// on-disk shape above: magic 4 + version u16 + generation u64 + 3×count u32.
const HEADER_LEN: usize = 4 + size_of::<u16>() + size_of::<u64>() + 3 * size_of::<u32>();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryCheckpoint {
    pub format_version: u16,
    pub generation: u64,
    pub identities: Vec<IdentityRecord>,
    pub replicas: Vec<ReplicaRecord>,
    pub placements: Vec<PlacementRecord>,
    // PR6-001 — the allocator floors at publication. Pruning deletes the
    // only historical source for old mutations (the orphan log's burned
    // generations, ckp009), so "decode succeeded" is not completeness: the
    // checkpoint must carry the floors, or a reopen recomputes them low and
    // reuses ids/generations. next_seq/next_segment_id stay derivable — the
    // prune never touches the WAL, manifest or segments their recovery
    // reads.
    pub next_logical_id: u64,
    pub next_replica_id: u64,
    pub next_placement_generation: u64,
    // PR6-R2-002 — per-family publication chains at publication time (the
    // chain_extend fold over every generation each family published at).
    // The coverage validator recomputes the fold over the surviving
    // post-checkpoint delta files and requires it to land on the CURRENT
    // manifest's chain — recovery depends on CURRENT + checkpoint +
    // authoritative deltas + WAL, never on a historical manifest.
    pub identity_chain: u64,
    pub replica_chain: u64,
    pub placement_chain: u64,
}

pub fn checkpoint_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("CHECKPOINT-{generation:06}.log"))
}

/// Parse the generation out of a `CHECKPOINT-{gen:06}.log` name.
pub fn checkpoint_generation(name: &str) -> Option<u64> {
    name.strip_prefix("CHECKPOINT-")
        .and_then(|s| s.strip_suffix(".log"))
        .and_then(|g| g.parse::<u64>().ok())
}

impl DirectoryCheckpoint {
    /// The live directories' snapshot at the Db's current generation.
    /// Records are SORTED by key (oid/lid/rid) — HashMap iteration order
    /// is random, and identical workloads must produce byte-identical
    /// checkpoints (the M35 determinism rule).
    // PR6-R2-002 — the ten arguments mirror the on-disk record shape 1:1;
    // grouping the counters/chains into a struct would only bury the format.
    #[allow(clippy::too_many_arguments)]
    pub fn from_state(
        generation: u64,
        identity: &HashMap<crate::identity::ObjectId, crate::identity::LogicalId>,
        replicas: &HashMap<crate::identity::LogicalId, crate::identity::ReplicaId>,
        placements: &HashMap<crate::identity::ReplicaId, Placement>,
        next_logical_id: u64,
        next_replica_id: u64,
        next_placement_generation: u64,
        identity_chain: u64,
        replica_chain: u64,
        placement_chain: u64,
    ) -> Self {
        let mut identities: Vec<IdentityRecord> = identity
            .iter()
            .map(|(&oid, &lid)| IdentityRecord { oid, lid })
            .collect();
        identities.sort_by_key(|r| r.oid);
        let mut replicas: Vec<ReplicaRecord> = replicas
            .iter()
            .map(|(&lid, &rid)| ReplicaRecord {
                lid,
                node: LOCAL_NODE_ID,
                rid,
            })
            .collect();
        replicas.sort_by_key(|r| r.lid);
        let mut placements: Vec<PlacementRecord> = placements
            .iter()
            .map(|(&rid, &placement)| PlacementRecord { rid, placement })
            .collect();
        placements.sort_by_key(|r| r.rid);
        DirectoryCheckpoint {
            format_version: FORMAT_VERSION,
            generation,
            identities,
            replicas,
            placements,
            next_logical_id,
            next_replica_id,
            next_placement_generation,
            identity_chain,
            replica_chain,
            placement_chain,
        }
    }

    /// P4-M6 — the one writer both paths use: header + sorted records
    /// stream to any `io::Write`, feeding the whole-file sha256
    /// incrementally (the checksum8 the decoder verifies) and appending its
    /// first 8 bytes last. No full encoded buffer — the publish path
    /// streams each fixed-width record (≤ 33 B) straight into the staged
    /// temp, so publishing a 1M-object checkpoint never allocates its
    /// ~81 MB encoded image.
    pub fn write_streamed(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
        let digest = {
            let mut hasher = Sha256::new();
            let mut w = |bytes: &[u8]| -> std::io::Result<()> {
                hasher.update(bytes);
                out.write_all(bytes)
            };
            w(CHECKPOINT_MAGIC)?;
            w(&self.format_version.to_le_bytes())?;
            w(&self.generation.to_le_bytes())?;
            w(&(self.identities.len() as u32).to_le_bytes())?;
            for r in &self.identities {
                w(r.oid.as_bytes())?;
                w(&r.lid.to_bytes())?;
            }
            w(&(self.replicas.len() as u32).to_le_bytes())?;
            for r in &self.replicas {
                w(&r.lid.to_bytes())?;
                w(&r.node.to_bytes())?;
                w(&r.rid.to_bytes())?;
            }
            w(&(self.placements.len() as u32).to_le_bytes())?;
            for r in &self.placements {
                w(&r.rid.to_bytes())?;
                match r.placement {
                    Placement::Memtable { generation } => {
                        w(&[1u8])?;
                        w(&[0u8; 16])?; // zeroed placement fields
                        w(&generation.to_le_bytes())?;
                    }
                    Placement::Segment(loc) => {
                        w(&[2u8])?;
                        w(&loc.segment_id.to_bytes())?;
                        w(&loc.block_id.to_bytes())?;
                        w(&loc.entry_offset.to_le_bytes())?;
                        w(&loc.generation.to_le_bytes())?;
                    }
                    Placement::Retired { generation } => {
                        w(&[3u8])?;
                        w(&[0u8; 16])?; // zeroed placement fields
                        w(&generation.to_le_bytes())?;
                    }
                }
            }
            // PR6-001 — the floors ride INSIDE the checksum, before it:
            // an old binary reading this file fails the checksum, and a new
            // binary reading an old file hits EOF before the checksum —
            // both directions fail closed without a FORMAT_VERSION bump.
            // PR6-R2-002 — the chains follow the floors, same rule.
            w(&self.next_logical_id.to_le_bytes())?;
            w(&self.next_replica_id.to_le_bytes())?;
            w(&self.next_placement_generation.to_le_bytes())?;
            w(&self.identity_chain.to_le_bytes())?;
            w(&self.replica_chain.to_le_bytes())?;
            w(&self.placement_chain.to_le_bytes())?;
            hasher.finalize()
        };
        out.write_all(&digest[..8])
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut cur = Cursor::new(bytes);
        if cur.take(4)? != CHECKPOINT_MAGIC {
            return Err(FormatError::Corrupt("checkpoint bad magic".into()));
        }
        let format_version = cur.u16()?;
        if format_version != FORMAT_VERSION {
            return Err(FormatError::Unsupported(format!(
                "checkpoint format version {format_version} (this build: {FORMAT_VERSION})"
            )));
        }
        let generation = cur.u64()?;
        // Plausibility caps before allocating (the manifest precedent):
        // fixed-width records, so a count that cannot fit is corruption.
        let identity_count = cur.u32()? as usize;
        if identity_count > cur.remaining() / IDENTITY_RECORD_LEN {
            return Err(FormatError::Corrupt(format!(
                "checkpoint identity_count {identity_count} cannot fit in {} remaining bytes",
                cur.remaining()
            )));
        }
        let mut identities = Vec::with_capacity(identity_count);
        for _ in 0..identity_count {
            let oid = crate::identity::ObjectId::from_bytes(
                cur.take(16)?.try_into().expect("16-byte slice"),
            );
            let lid = crate::identity::LogicalId::from_bytes(
                cur.take(8)?.try_into().expect("8-byte slice"),
            );
            identities.push(IdentityRecord { oid, lid });
        }
        let replica_count = cur.u32()? as usize;
        if replica_count > cur.remaining() / REPLICA_RECORD_LEN {
            return Err(FormatError::Corrupt(format!(
                "checkpoint replica_count {replica_count} cannot fit in {} remaining bytes",
                cur.remaining()
            )));
        }
        let mut replicas = Vec::with_capacity(replica_count);
        for _ in 0..replica_count {
            let lid = crate::identity::LogicalId::from_bytes(
                cur.take(8)?.try_into().expect("8-byte slice"),
            );
            let node = NodeId::from_bytes(cur.take(8)?.try_into().expect("8-byte slice"));
            let rid = crate::identity::ReplicaId::from_bytes(
                cur.take(8)?.try_into().expect("8-byte slice"),
            );
            replicas.push(ReplicaRecord { lid, node, rid });
        }
        let placement_count = cur.u32()? as usize;
        if placement_count > cur.remaining() / PLACEMENT_RECORD_LEN {
            return Err(FormatError::Corrupt(format!(
                "checkpoint placement_count {placement_count} cannot fit in {} remaining bytes",
                cur.remaining()
            )));
        }
        let mut placements = Vec::with_capacity(placement_count);
        for _ in 0..placement_count {
            let rid = crate::identity::ReplicaId::from_bytes(
                cur.take(8)?.try_into().expect("8-byte slice"),
            );
            let variant = cur.u8()?;
            let placement = match variant {
                1 => {
                    let _ = cur.take(16)?; // zeroed placement fields
                    Placement::Memtable {
                        generation: cur.u64()?,
                    }
                }
                2 => Placement::Segment(crate::placement::directory::PhysicalLocation {
                    segment_id: SegmentId::from_bytes(
                        cur.take(8)?.try_into().expect("8-byte slice"),
                    ),
                    block_id: BlockId::from_bytes(cur.take(4)?.try_into().expect("4-byte slice")),
                    entry_offset: cur.u32()?,
                    generation: cur.u64()?,
                }),
                3 => {
                    let _ = cur.take(16)?; // zeroed placement fields
                    Placement::Retired {
                        generation: cur.u64()?,
                    }
                }
                other => {
                    return Err(FormatError::Unsupported(format!(
                        "checkpoint placement variant byte {other}"
                    )));
                }
            };
            placements.push(PlacementRecord { rid, placement });
        }
        let next_logical_id = cur.u64()?;
        let next_replica_id = cur.u64()?;
        let next_placement_generation = cur.u64()?;
        let identity_chain = cur.u64()?;
        let replica_chain = cur.u64()?;
        let placement_chain = cur.u64()?;
        let stored_ck = cur.take(8)?;
        if !cur.is_empty() {
            return Err(FormatError::Corrupt("checkpoint trailing bytes".into()));
        }
        if checksum8(&bytes[..bytes.len() - 8]) != stored_ck {
            return Err(FormatError::Corrupt("checkpoint checksum mismatch".into()));
        }
        Ok(DirectoryCheckpoint {
            format_version,
            generation,
            identities,
            replicas,
            placements,
            next_logical_id,
            next_replica_id,
            next_placement_generation,
            identity_chain,
            replica_chain,
            placement_chain,
        })
    }

    /// P4-M6 — publish through the streaming writer: the same staging
    /// protocol (temp write → crash parks → fsync → rename), no encoded
    /// buffer. The parks fire identically, so the M40 crash windows cover
    /// the streamed path unchanged.
    /// PR6-006 — this is the ONLY publish API: the materialized
    /// encode()/publish_staged() forms are gone, so a production caller
    /// cannot accidentally materialize a large checkpoint.
    pub fn publish_staged_streamed(
        path: &Path,
        checkpoint: &Self,
        stage: Option<&str>,
    ) -> Result<(), FormatError> {
        publish_atomic_writer_staged(path, stage, |f| checkpoint.write_streamed(f))
    }

    /// Read back + decode — the verify-publication step (review P0-2 step 7):
    /// the history is only pruned after the checkpoint PROVES decodable.
    pub fn read(path: &Path) -> Result<Self, FormatError> {
        let bytes = std::fs::read(path)
            .map_err(|e| FormatError::Io(format!("read checkpoint {}: {e}", path.display())))?;
        Self::decode(&bytes)
    }
}

/// The newest valid checkpoint at or below `current_generation` — None when
/// no checkpoint exists (recovery falls back to the full delta history).
/// A file that fails to decode fails closed (see the module doc's damage
/// policy). A file whose internal generation disagrees with its name is a
/// publication anomaly — fail closed, never pick.
pub fn load_newest(
    dir: &Path,
    current_generation: u64,
) -> Result<Option<DirectoryCheckpoint>, FormatError> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)
        .map_err(|e| FormatError::Io(format!("read checkpoints in {}: {e}", dir.display())))?
        .flatten()
    {
        let name = entry.file_name();
        let Some(gen) = checkpoint_generation(&name.to_string_lossy()) else {
            continue;
        };
        if gen <= current_generation && best.as_ref().is_none_or(|(g, _)| gen > *g) {
            best = Some((gen, entry.path()));
        }
    }
    let Some((gen, path)) = best else {
        return Ok(None);
    };
    let checkpoint = DirectoryCheckpoint::read(&path)?;
    if checkpoint.generation != gen {
        return Err(FormatError::Corrupt(format!(
            "CHECKPOINT-{gen:06}.log carries generation {}",
            checkpoint.generation
        )));
    }
    Ok(Some(checkpoint))
}

/// PR6-002 — post-checkpoint delta coverage (review P0 Recovery): a valid
/// checkpoint plus an incomplete delta set is an invalid state, so the open
/// fails closed when a REQUIRED authoritative delta generation is missing.
/// Gaps are normal (a generation with no work for a family publishes no
/// log) and raise no requirement; a missing INTERMEDIATE log (the review's
/// PLACEMENT-113 with CURRENT=120) fails exactly like a missing newest
/// one, because the records in between are nowhere else. No checkpoint ⇒
/// no baseline to be incomplete relative to — the scan is a no-op.
///
/// PR6-R2-002 (review P0 Recovery) — the scan reads NO historical
/// manifest. The invariant:
///
/// > Recovery must depend only on CURRENT + checkpoint + authoritative
/// > post-checkpoint deltas + WAL, not on an unbounded chain of manifests.
///
/// The checkpoint carries the per-family publication chains (the
/// `chain_extend` fold over every generation each family published at);
/// CURRENT's manifest carries the running folds. The validator scans the
/// post-checkpoint delta files (the directory is the index), re-folds their
/// generations from the checkpoint's base and requires the fold to land on
/// CURRENT's manifest exactly — a deleted intermediate log breaks the fold
/// — while the floor agreement names a missing NEWEST log precisely.
pub fn validate_delta_coverage(
    dir: &Path,
    checkpoint: Option<&DirectoryCheckpoint>,
    manifest: &Manifest,
) -> Result<(), FormatError> {
    let Some(ckp) = checkpoint else { return Ok(()) };
    if ckp.generation >= manifest.generation {
        return Ok(());
    }
    validate_family_coverage(
        dir,
        "IDENTITY",
        ckp.generation,
        manifest.generation,
        ckp.identity_chain,
        manifest.identity_chain,
        manifest.identity_floor,
    )?;
    validate_family_coverage(
        dir,
        "REPLICA",
        ckp.generation,
        manifest.generation,
        ckp.replica_chain,
        manifest.replica_chain,
        manifest.replica_floor,
    )?;
    validate_family_coverage(
        dir,
        "PLACEMENT",
        ckp.generation,
        manifest.generation,
        ckp.placement_chain,
        manifest.placement_chain,
        manifest.placement_floor,
    )
}

/// PR6-R2-002 — the scan body: the authoritative post-checkpoint delta
/// files of one family (the directory listing IS the index — every file in
/// range must decode with its generation agreeing with its name, the
/// loaders' agreement rule), re-folded into the publication chain. The
/// sorted fold is generation-monotone by construction; the chain equality
/// catches a missing intermediate log, the floor agreement a missing newest
/// one (and names it).
fn validate_family_coverage(
    dir: &Path,
    family: &str,
    checkpoint_generation: u64,
    current_generation: u64,
    base_chain: u64,
    current_chain: u64,
    current_floor: u64,
) -> Result<(), FormatError> {
    let mut gens: Vec<u64> = std::fs::read_dir(dir)
        .map_err(|e| FormatError::Io(format!("scan delta logs in {}: {e}", dir.display())))?
        .flatten()
        .filter_map(|entry| {
            let g = match family {
                "IDENTITY" => identity_log_generation(&entry.file_name().to_string_lossy()),
                "REPLICA" => crate::identity::directory::replica_log_generation(
                    &entry.file_name().to_string_lossy(),
                ),
                _ => crate::placement::directory::placement_log_generation(
                    &entry.file_name().to_string_lossy(),
                ),
            }?;
            (g > checkpoint_generation && g <= current_generation).then_some(g)
        })
        .collect();
    gens.sort_unstable();
    let mut chain = base_chain;
    for &g in &gens {
        require_delta(dir, family, g)?;
        chain = chain_extend(chain, g);
    }
    if current_floor > checkpoint_generation && gens.last().copied() != Some(current_floor) {
        return Err(FormatError::Corrupt(format!(
            "required authoritative delta {family}-{current_floor:06}.log missing (post-checkpoint coverage broken)"
        )));
    }
    if chain != current_chain {
        return Err(FormatError::Corrupt(format!(
            "post-checkpoint {family} delta coverage incomplete (a published log is missing)"
        )));
    }
    Ok(())
}

/// The required `{FAMILY}-{g:06}.log` must exist and decode with its
/// generation agreeing with its name (the loaders' agreement rule, per
/// file). Missing is Corrupt, not Io — the recovery SET on disk is invalid,
/// which is semantic, not an OS failure.
fn require_delta(dir: &Path, family: &str, g: u64) -> Result<(), FormatError> {
    let path = match family {
        "IDENTITY" => identity_log_path(dir, g),
        "REPLICA" => replica_log_path(dir, g),
        _ => placement_log_path(dir, g),
    };
    if !path.exists() {
        return Err(FormatError::Corrupt(format!(
            "required authoritative delta {family}-{g:06}.log missing (post-checkpoint coverage broken)"
        )));
    }
    let bytes = std::fs::read(&path)
        .map_err(|e| FormatError::Io(format!("read {}: {e}", path.display())))?;
    let gen = match family {
        "IDENTITY" => IdentityLog::decode(&bytes)?.generation,
        "REPLICA" => ReplicaLog::decode(&bytes)?.generation,
        _ => PlacementLog::decode(&bytes)?.generation,
    };
    if gen != g {
        return Err(FormatError::Corrupt(format!(
            "{family}-{g:06}.log carries generation {gen}"
        )));
    }
    Ok(())
}

/// Delete every directory delta log at or below `generation` (fully
/// subsumed by the checkpoint now published at that generation) and every
/// OLDER checkpoint. Deletion failures warn — a leftover is harmless (the
/// checkpoint answers first; re-applying old logs is idempotent under the
/// merge gates). Parks after the first deletion for the crash matrix
/// (`AIKOQL_V2_CKP_PARK` = `after_first_prune`). Returns files removed.
pub fn prune_deltas_before(dir: &Path, generation: u64) -> Result<u32, FormatError> {
    let mut deleted: u32 = 0;
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| FormatError::Io(format!("prune directory logs in {}: {e}", dir.display())))?
        .flatten()
    {
        names.push(entry.file_name());
    }
    for name in names {
        let name = name.to_string_lossy();
        let log_gen = identity_log_generation(&name)
            .or_else(|| crate::identity::directory::replica_log_generation(&name))
            .or_else(|| crate::placement::directory::placement_log_generation(&name));
        let remove = match log_gen {
            Some(gen) => gen <= generation,
            None => checkpoint_generation(&name).is_some_and(|gen| gen < generation),
        };
        if !remove {
            continue;
        }
        let path = dir.join(name.as_ref());
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!(
                "aikoql-v2: obsolete directory file {} not removed: {e}",
                path.display()
            );
            continue;
        }
        deleted += 1;
        if deleted == 1 {
            crash_park("AIKOQL_V2_CKP_PARK", dir, "after_first_prune");
        }
    }
    Ok(deleted)
}

/// Total bytes of directory delta logs published after `after_generation`
/// (every generation, orphan windows included — orphan bytes will be
/// re-published, so they count toward the next checkpoint). The Db seeds
/// its checkpoint budget from this at open and adds each log it publishes.
pub fn directory_log_bytes(dir: &Path, after_generation: u64) -> Result<u64, FormatError> {
    let mut bytes: u64 = 0;
    for entry in std::fs::read_dir(dir)
        .map_err(|e| FormatError::Io(format!("sum directory logs in {}: {e}", dir.display())))?
        .flatten()
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let gen = identity_log_generation(&name)
            .or_else(|| crate::identity::directory::replica_log_generation(&name))
            .or_else(|| crate::placement::directory::placement_log_generation(&name));
        let Some(gen) = gen else { continue };
        if gen <= after_generation {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            bytes += meta.len();
        }
    }
    Ok(bytes)
}

/// PR6-006 — the ONLY materialization surface, explicitly named so no
/// production caller can claim it was an accident. `DirectoryCheckpoint`
/// itself offers no `encode()`/materialized publish: the production API is
/// `publish_staged_streamed` alone, and these functions exist purely as the
/// byte-identity reference the test pins compare the streamed file against.
#[doc(hidden)]
pub mod test_support {
    use super::{
        DirectoryCheckpoint, HEADER_LEN, IDENTITY_RECORD_LEN, PLACEMENT_RECORD_LEN,
        REPLICA_RECORD_LEN,
    };

    /// The materialized reference form — write_streamed into a Vec (the one
    /// writer, so this can never drift from the published bytes).
    pub fn encode_for_tests(cp: &DirectoryCheckpoint) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(
            HEADER_LEN
                + cp.identities.len() * IDENTITY_RECORD_LEN
                + cp.replicas.len() * REPLICA_RECORD_LEN
                + cp.placements.len() * PLACEMENT_RECORD_LEN
                + 8,
        );
        // A Vec write cannot fail.
        cp.write_streamed(&mut bytes)
            .expect("write to Vec cannot fail");
        bytes
    }
}
