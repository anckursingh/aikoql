# MRFC-0080 — Replication (roadmap Phase 2)

Status: R0 accepted, R1 done (RED `r1-replication-log`), R1b done (pin; mutant `r1b-bootstrap-id-floor-mutant`). Branch `feature/aikoql-db-launch`.
Gate (roadmap Phase 2): crash-window certs on a 2-node setup.

## Decision

Single-writer **leader → follower physical log shipping** with term
fencing. Not Raft: one writer needs a lease + fencing, not consensus
(Raft re-opens only for multi-writer — Phase 3).

Followers apply the leader's storage WAL frames byte-for-byte; they never
re-execute transactions. Re-execution is non-deterministic (embedding
provider calls, commit timestamps, KOID assignment, conflict detection)
and would fork the hash-chained audit. Physical shipping keeps a follower
an exact copy, which the verifiable-knowledge claim requires.

## What already exists (reused, not rebuilt)

| piece | where | role |
|---|---|---|
| sequenced, checksummed per-transaction WAL frames | `aikoql-v2/src/wal.rs` | the replication unit — one kernel txn = one frame |
| identity triple rides `OP_CREATE_OBJECT` | `wal.rs` | followers copy ids, never allocate |
| derived indexes rebuild + resume from a watermark | `IndexMaintainer`, `rebuild_derived_indexes` | ship storage only; followers derive text/vector/property locally |
| unknown frame type → `Unsupported` | `wal.rs` decode order | old binaries refuse new frames, never mis-apply |
| framed binary protocol | `crates/protocol/native` | R3 transport |
| fault proxy | `crates/sdk/fault-proxy` | R3/R4 partition + reset matrix |

## Gaps and where each closes

| gap | closes in |
|---|---|
| WAL truncated at every flush — a lagging follower has nothing to catch up from | R1 |
| no "frames after seq N" read surface | R1 |
| no apply path that keeps the leader's seq and advances the id allocators | R1 |
| follower bootstrap (snapshot export/import) | R1b ✅ |
| kernel has no read-only follower mode; in-memory state (journal head, caches, tenant directory) must follow applied frames | R2 |
| transport, lag metrics, read-your-writes via seq tokens | R3 |
| semi-sync ack (RPO = 0) and manual promotion with term fencing | R4 |
| automatic failover via an external lease (k8s/etcd or S3 conditional put) | R5 |
| log + snapshot archive to S3 (PITR) — the enterprise S3 leg | R6 |
| state outside the DB: KMS master key file (`security/kms.rs`, needed by followers — frames are ciphertext), MCP tool-call audit log (`mcp/src/audit.rs`), backup `meta.json` | R2 (documented per-node or moved in) |

## R1 — storage replication log (`aikoql-v2`) ✅

- `Config::retain_log` (default false). When on, flush phase C first
  publishes the covered WAL prefix to `rlog/<first_seq>.wal` (tmp →
  fsync → rename → dir fsync) and only then truncates the WAL. A crash
  between the two leaves the frames in both places; readers dedup by seq.
- `Db::log_since(after, max) -> Vec<WalFrame>`: frames with seq > `after`
  from the archive then the active WAL, contiguous from `after + 1`. A gap
  (frames pruned, or written before `retain_log` was on) is an error —
  the follower must re-bootstrap (R1b).
- `Db::release_log(upto)`: drops archive files wholly at or below `upto`
  (the minimum follower-acked seq). Size-capped retention arrives with R3.
- `Db::apply_replicated(seq, ops)`: Sync mode only. `seq < next` is an
  idempotent no-op (re-delivery), `seq > next` is a gap error, `seq ==
  next` writes the frame under the leader's seq. `CreateObject` advances
  the lid/rid/placement-generation allocators past the copied ids so a
  promoted follower never reuses one (the same floors open() recovers).
- `Db::last_seq()`: the follower's resume point after a restart.

Divergence detection (R4) needs no format change: a reconnecting follower
sends `(last_seq, frame checksum)`; a mismatch means its tail diverged and
is truncated to the new leader's history.

## Tests (RED first, per AGENTS.md)

- R1: `crates/storage/aikoql-v2/tests/replication_log.rs` — leader with
  forced flushes ships to a follower; byte keys and object rows agree,
  follower ids never collide after promotion, re-delivery is a no-op, a
  gap fails closed, `release_log` prunes, follower restart resumes.

## R1b — follower bootstrap ✅

No new code: `leader.snapshot_to(dir)` (P5-M33, crash-protocol pinned)
then `restore_from(dir, follower_dir)` yields a follower at a consistent
`last_seq`; `log_since(follower.last_seq())` catches it up. Required when
the leader's early history was truncated before `retain_log` or released.
Pinned by `tests/replication_bootstrap.rs`. R3 moves the snapshot
directory over the wire; locally it is a directory copy.

## R1 known limits (shortcut comments in `db.rs`)

- The archive copy runs inside flush phase C under the WAL lock; writers
  wait one memtable's worth of WAL bytes. Move it to phase B if measured.
- `log_since` reads every archive file per call; seek by file name once
  catch-up volume matters (R3 batches calls with `max`).
- `apply_replicated` requires `DurabilityMode::Sync` on the follower.
- No crash-window cert yet for the archive → truncate window (dedup by seq
  makes it safe by construction); it lands with the R4 2-node certs.
