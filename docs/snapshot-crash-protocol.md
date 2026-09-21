# Engine-native snapshot — crash protocol

The contract behind `Db::snapshot_to` / `restore_from`
(crates/storage/aikoql-v2/src/snapshot.rs). Grep-pinned by
snapshot_redesign.rs (snp000). Two protocol generations are documented:
the shipped P3-M3 protocol and the M33 redesign. The RED tests for M33
cite the sections below.

## Old protocol (P3-M3, PR6-007 matrix)

`snapshot_to` held the state READ lock and the wal mutex across the
whole capture → copy → verify → marker span. Writers blocked for the
snapshot's entire duration. The matrix pinned:

- kill mid-copy → unmarked dir, restore refuses (sfm005)
- kill after the marker commit → restorable (sfm006)
- torn WAL tail → only the torn-safe prefix rides along (sfm007)
- interleaved write/flush/checkpoint/compaction land only after the
  snapshot completes (sfm001–004, 003b)

## New protocol (M33 — the pin)

**Capture** (state READ lock, held for a fast metadata-only window):
read CURRENT → generation G, enumerate the pinned file set (CURRENT,
MANIFEST-G, the segments it references, every identity/replica/
placement/checkpoint log ≤ G, the WAL), and measure the WAL's
torn-safe prefix length under the wal mutex (scan only — no copying).

**Arm the pin** (state WRITE lock): insert the file set into
`snapshot_pins`. From here until disarm, both deletion surfaces —
`prune_deltas_before` and the segment deletion in the flush/compact
path — skip pinned names. Everything else remains prunable.

**Copy + verify + marker — no locks held.** The files ≤ G are
immutable under the publication protocol (every publish is a staged
rename, never an in-place edit). Two exceptions are handled without
locks:

- CURRENT is mutable (rewritten by every checkpoint): the snapshot
  SYNTHESIZES its bytes from G instead of re-reading the live file, so
  a concurrent checkpoint can never tear the snapshot's CURRENT away
  from its MANIFEST (the "mix" is impossible by construction).
- The WAL is append-only: the captured prefix length fixes the cut;
  the copy reads exactly that many bytes through its own handle.
  Frames acked after the capture are beyond the cut and stay
  invisible to the snapshot.

**Disarm the pin** (state WRITE lock), on success and on every error
path (a guard's Drop). A leaked pin is a slow disk leak — files the
pin protects would never be pruned.

**Writers during the snapshot**: a put lands while the copy is in
flight; its WAL frame is beyond the captured prefix, so the snapshot
keeps the P3-M3 semantics — the write is invisible to it and visible
to the live db immediately.

## Failure windows (new protocol)

- **Kill mid-copy**: the marker is published last, so a kill leaves an
  unmarked dir — inert, restore refuses (sfm005 semantics). The pin is
  in-memory and dies with the process; any files it protected remain as
  harmless leftovers (the same tolerated class as the existing
  deletion-failure leftovers).
- **Kill after the marker commit**: restorable (sfm006 semantics,
  unchanged).
- **Checkpoint during the copy**: lands while the snapshot is in
  flight; publishes CHECKPOINT-G and prunes every unpinned log ≤ G.
  Pinned files survive for the snapshot's copy; the snapshot dir holds
  only files ≤ G (never a mix).
- **Compaction during the copy**: merges and deletes live segments
  except the pinned ones; the snapshot's copies complete and restore
  byte-exact.
- **Crash of the LIVE db during a snapshot**: the snapshot dir is a
  separate directory; the live db's own recovery is the
  checkpoint/compaction crash protocol, unchanged by the pin (the pin
  has no durable form).
- **Snapshot error mid-copy** (Io on a file, verify mismatch): the pin
  disarms via the guard; a partial snapshot dir is unmarked and inert.
