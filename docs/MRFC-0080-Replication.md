# MRFC-0080 — Replication (roadmap Phase 2)

Status: R0 accepted, R1 done (RED `r1-replication-log`), R1b done (pin; mutant `r1b-bootstrap-id-floor-mutant`), R2 done (RED `r2-promotion-koid-collision`), R3a + R3b done (REDs `r3a-replication-stream`, `r3b-promotion-full-reindex`, `r3b-empty-hnsw-checkpoint`). Branch `feature/aikoql-db-launch`.
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
| promotion: a kernel opened over the standby's storage must continue the leader's state and never re-mint a committed KOID | R2 ✅ |
| follower reads (read replicas): a live kernel whose in-memory state follows applied frames | later (needs a write-free kernel open) |
| transport + manual failover | R3a ✅ |
| failover time: promotion rebuilt every index from the whole journal | R3b ✅ |
| TLS for cross-host links; lag metrics endpoint | R3c |
| terms + fencing in the data path | R4a ✅ |
| semi-sync ack (RPO = 0 for acked writes) | R4b ✅ |
| diverged-tail rejoin (and automatic re-bootstrap) | R4c ✅ |
| `NOT_LEADER` | R4d |
| automatic failover: lease agent (S3 first, then k8s), self-fencing, in-process promotion, client leader discovery | R5 (designed below) |
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

Divergence detection landed in R4c by term history, not by frame
checksum: a standby without `retain_log` cannot checksum frames it has
already flushed, and the LSM cannot truncate a committed tail in place.

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

## R2 — standby + promotion through the kernel ✅

Design, narrowed from the analysis: the standby is a bare `Db` applying
frames (R1) — it runs no kernel, so it cannot accept a write by
construction. Promotion = drop the standby `Db`, open the kernel over its
directory. `Kernel::open` already rebuilds every in-memory structure from
storage (journal head + audit chain, HLC re-seed, auth, schemas, jobs,
catalog, property indexes) — the crash-recovery path, so promotion is
correct by the same certification as a restart.

The one defect found and fixed: **KOID re-issue**. A KOID is
`clock ms | counter | id_seed`; `IdGen` restarted at (ms 0, counter 0)
on every open and the MCP seed is fixed (`0xA9C9`), so a node opened in
the same millisecond as, or with a clock behind, the leader's commits
minted an existing KOID — `VersionConflict` on create (OCC kept the data;
every create failed until the clock passed the history). Fix:
`Kernel::open` calls `IdGen::skip_past(last_ts >> 16)` — a committed
KOID's ms is ≤ its commit's HLC ms. The same path also hit single-node
restarts after a backward clock step.

`AikoqlStorageEngineV2::db()` exposes the Db so a kernel-driven leader
serves `log_since` while it writes. Pinned by
`tests/replication_kernel.rs` (same-ms + lagging-clock promotion, live
shipping across flushes with update + tombstone, then promotion).

R2 limits, carried forward:
- Derived indexes (text/vector/property) are rebuilt at promotion — the
  standby has no `{db}.ckpt`; failover time grows with data. R3 runs the
  `IndexMaintainer` on the standby (or ships its checkpoint).
- The standby needs the leader's KMS master key (`security/kms.rs`)
  provisioned out of band: frames carry ciphertext.
- The MCP tool-call audit file (`mcp/src/audit.rs`) and backup
  `meta.json` stay per node — the in-DB hash-chained audit ships.
- A promoted node must reopen with `retain_log` to feed new followers
  (R3 server config).

## R3a — the replication stream ✅

`aikoql-mcp serve --replicate-listen ADDR --tcp-token T::admin <db>` runs
the leader side beside any serve mode (stdio included) and turns on log
retention; `aikoql-mcp follow <standby> --leader ADDR` (token from
`--token` or `AIKOQL_REPLICATION_TOKEN`) runs the standby.

Wire `aikoql-repl/1` (`crates/services/api/mcp/src/replication.rs`): a
JSON handshake line (proto, token, resume seq) answered `{"ok":true}` or
`{"error":..}`; then tagged messages — raw checksummed WAL frames,
1 s heartbeats, a gap notice, index checkpoints — down, u64 acks up. The
leader releases its archive to the slowest **connected** standby's ack.
Standbys reconnect with backoff and resume from `last_seq`; refusals,
undecodable frames are fatal; gaps re-bootstrap automatically (R4c). Every
length read off the wire is bounded.

Security: standbys need an `admin` token (the stream is every tenant's
data); the listener is loopback-only and refuses to start without
tokens — the `--listen` rules. Cross-host links need a tunnel until R3c.

## R3b — fast failover ✅

Failover time was dominated by the derived indexes: the leader
checkpointed them only at shutdown, so after a crash the promoted node
replayed the whole journal. Now:

- the leader checkpoints every `--index-checkpoint-secs` (default 30);
  one lock serializes checkpoint writers and the sender (publication is
  remove + rename);
- a caught-up sender ships the checkpoint whenever its water moves —
  frames to the head precede it on the wire, so the water is already
  applied on the standby;
- the standby writes `{dir}.ckpt.recv` (paths validated: plain relative
  components only) and swaps it in as `{dir}.ckpt`, which `serve` resumes.

The pin found an older defect on the same path: an empty HNSW (no
embedding model) checkpoint could not be loaded (`fast-hnsw`: "file
contains no payload section"), so **every** restart — single-node too —
replayed the journal. Fixed: `physical == 0` loads as a fresh index.

Measured (`replication_failover.rs`, structural — events replayed by the
maintainer at promotion): **156 → 0** (history 150 + tail 5; the tail is
bounded by one checkpoint interval). The resume-fallback warning now
names its cause.

### Failover runbook (manual, until R4/R5)

1. Confirm the leader is down (a still-running leader keeps accepting
   writes the standby will never see — R4 adds fencing).
2. Stop `follow` on the standby (it is crash-safe; kill is fine).
3. `aikoql-mcp serve --replicate-listen ADDR --tcp-token … <standby>` —
   the kernel opens over the standby's storage, the indexes resume from
   `<standby>.ckpt`, and new standbys can follow it.
4. Point clients at the new leader. Bring the old leader back with
   `aikoql-mcp follow <old-dir> --leader <new-leader>` (R4c: a diverged
   tail is detected and replaced automatically) — never restart it as a
   leader.

R3 limits, carried forward:
- No fencing: a partitioned old leader can still take writes (R4a
  narrows this — see below).
- Async by default: writes acked in the last moments before a crash may
  not have shipped. `--replicate-sync-ms` (R4b) closes this.
- Checkpoints ship whole each time their water moves; incremental by
  file once indexes are large (shortcut comment in `send_checkpoint`).
- Release follows connected standbys only: a standby away longer than
  the retained log re-bootstraps (automatically since R4c — a full
  snapshot copy).

## R4a — terms and write fencing ✅

- Each node keeps `<dir>/REPL-TERM` = `"<term> <role>"`, role ∈
  `leader | standby | fenced`; no file = term 0, leader. Written tmp →
  fsync → rename → dir fsync: a lost write never unfences a node.
- Handshake: the hello carries the standby's term, `{"ok":true,"term"}`
  the leader's. The standby refuses a leader whose term is older than
  its own and adopts (persists) a newer one as `standby`.
- A leader that meets a newer term from a standby persists `fenced`,
  calls `StorageAdminApi::fence` (`Db::fence` → every client write fails
  with `stale: not leader: …`; replicated frames still apply) and
  refuses the connection. A fenced node serves no stream.
- `serve` on a `standby` directory is its promotion: term + 1, `leader`.
  `serve` on a `fenced` directory starts fenced — reads only — until the
  node rejoins with `follow` (which makes it a `standby` again).

Tests: `replication.rs` unit matrix (refuse + fence, adopt) and
`replication_failover.rs` (promotion writes `1 leader`; a fenced node
refuses writes after a restart). RED: `r4a-term-fencing{,-e2e}`.

R4a limits:
- A deposed leader is fenced only when a newer-term node **reaches** it.
  A partitioned old leader nobody connects to keeps taking writes until
  the lease (R5a) self-fences it — manual promotion still needs the
  runbook's step 1.
- `Kernel::open` on a fenced directory still writes its open-time
  records before the fence goes up; they are part of the diverged tail
  R4c replaces.

## R4b — synchronous replication acks ✅

- `serve --replicate-listen … --replicate-sync-ms MS`: a client write
  returns only once a standby has acked its seq (the standby acks after
  `apply_replicated` + fsync), or fails `not replicated` after MS.
- Mechanism: `Db::set_commit_gate` (via `StorageAdminApi`) runs a
  callback after each client write commits, outside the state/WAL locks;
  replicated frames never wait. The leader's gate waits on `Acked` — the
  highest seq any standby acked (Mutex + Condvar, fed by each standby's
  ack thread).
- Guarantee: **no acked write is lost to a failover onto a caught-up
  standby (RPO = 0 for acked writes).** A write that times out is
  committed on the leader but never acked — the client must treat it as
  unknown; if the leader then dies it belongs to the diverged tail R4c
  replaces.

Tests: `replication.rs` (every acked write is already on the standby;
no standby → `not replicated` after the timeout) and
`replication_failover.rs` (`--replicate-sync-ms 300` refuses to ack with
no standby, acks once one follows). RED: `r4b-semi-sync{,-e2e}`.

R4b limits:
- Strict, not degrading: with no standby connected every write waits the
  full timeout and fails. Run two standbys for write availability; a
  quorum count (`ack from N of M`) is a later option.
- Reads are not gated: other clients may read a write before a standby
  holds it (the same window as MySQL semi-sync / Postgres `remote_write`).
- Throughput under sync is bounded by commit-to-ack latency per writer;
  the 10 ms log poll in the sender is now on the write path (shortcut
  comment in `serve_standby` — a commit notify replaces it).

## R4c — diverged-tail rejoin ✅

- **Term history** (Postgres timelines): `<dir>/REPL-HISTORY` holds one
  `"<term> <first seq>"` line per promotion; no file = `0 1`. `serve` on
  a standby appends `(term + 1, last_seq + 1)` **before the kernel
  opens**, so the kernel's open-time writes belong to the new term.
  Standbys copy the leader's history at each handshake.
- **Detection** (leader, at handshake; protocol `aikoql-repl/2`): the
  hello carries the standby's history and `after`. `diverged(ours,
  theirs, after)` — the standby's last frame was written under term T
  (from its history); it is consistent iff we had T and had not moved
  past T before `after`. A standby ahead of our head is diverged too.
- **Repair**: no in-place truncation (the LSM cannot rewind a committed
  tail). The leader snapshots its storage (`snapshot_to` into
  `<dir>.ship-<id>`) and ships it as tag `5`, the checkpoint directory
  format; the standby lands it in `<dir>.rejoin`. `follow` returns
  `Followed::Rebootstrap`; the CLI closes the db and `rebootstrap`
  restores into `<dir>.new` with the leader's term + history, swaps it in
  (old data and the stale `.ckpt` removed) and follows on from the
  snapshot's seq.
- The same path replaces R3a's fatal gap: a standby whose position the
  retained log no longer holds — including a fresh one — re-bootstraps
  without an operator.
- Term and history are written only with the data they describe: a
  diverged node keeps its old history until the swap, so a crash
  mid-rejoin is detected again on the next connect.

Tests: `replication.rs` — `diverged` table, the drill (A leads, B
follows, A takes a tail and dies, B is promoted and writes, A follows B:
re-bootstrapped, no tail survives, scans equal), a caught-up old leader
rejoins **without** a snapshot, a gap re-bootstraps automatically;
`replication_failover.rs` — the drill through `serve` + `follow`. RED:
`r4c-diverged-rejoin{,-e2e}`.

R4c limits:
- Repair is a full snapshot copy, sized by the database, not by the
  tail. Fine for the rare rejoin; an incremental rewind needs seq-aware
  truncation in storage.
- Two operators promoting two standbys to the same term (manual split
  brain) share a term number with different histories; detection
  compares terms, not their first seqs. The R5 lease makes one winner
  per term by construction.
- A crash between `rebootstrap`'s two renames leaves no `<dir>`; the
  next `follow` starts empty and re-bootstraps again (the old data is in
  `<dir>.old` until then).
- Under heavy writes the leader may release log past the snapshot's seq
  before the standby reconnects (release follows connected standbys);
  it then re-bootstraps once more.

## R5 — automatic failover (design, accepted; builds on R4)

Detection is the easy half; **fencing** is the hard half. Automating
promotion without a guarantee that the old leader has stopped writing
turns every network partition into two leaders with diverging histories
— the worst outcome for a verifiable-knowledge database. R5 therefore
sits on R4 (terms enforced in the data path) and adds a lease.

### Model: one lease, one term (Patroni-style)

A single record in an external store holds
`{leader, term, address, expires}`. A node becomes leader **only** by a
conditional write (compare-and-swap) that bumps `term`. Every node runs
`serve` with an HA flag and the lease location; an in-process agent
does the rest.

| lease backend | fits | extra infra | order |
|---|---|---|---|
| S3 conditional writes (`If-Match` / `If-None-Match`) | the enterprise S3 tier, any cloud | none beyond S3 | R5a |
| Kubernetes `Lease` object | k8s deployments | none on k8s | R5b |
| etcd / Consul | on-prem shops already running them | a cluster | later |

S3 first: it is the dependency the enterprise storage tier already
brings, so HA costs no new infrastructure.

### Rules

1. **Self-fencing (leader).** Renew every TTL/3 (e.g. TTL 10 s → 3 s).
   Unable to renew within `TTL − drift margin`, the leader stops
   accepting writes on its own — it cannot rely on being told during a
   partition. Hence: when a standby may take the lease, the old leader
   has already stopped writing.
2. **Takeover (standby).** Only after the lease has expired — judged by
   the store's clock where the backend offers one — and only when the
   standby is current: with R4's semi-sync every acked write is already
   on it (RPO = 0). Win the CAS with `term + 1`, then promote
   **in-process** (stop applying → open the kernel → serve); today
   `follow` and `serve` are separate processes, so the agent adds the
   mode switch.
3. **Terms in the data path (R4).** The term rides the replication
   handshake; every node persists the highest term it has seen; frames
   and writes from a lower term are refused. A returning old leader
   compares `(last_seq, frame checksum)` with the new leader, truncates
   its diverged tail, and rejoins as a follower without operator help.
4. **Client discovery.** Followers answer writes with `NOT_LEADER` +
   the leader's address (from the lease); the SDKs follow the hint and
   retry. A leader-updated DNS name or a thin proxy is the alternative.
   Without one of these, failover is automatic for the database and
   manual for every application.

**Expected failover time:** ≈ TTL (10 s) + promotion (< 1 s since R3b
removed the index rebuild) → ~10–15 s, tunable through the TTL.

### Alternatives considered

- **Raft / built-in consensus** — no external store, but three voters
  and the largest build; pays off only with multi-writer (Phase 3).
- **Two data nodes + a witness voter** (MongoDB-arbiter style) — a
  2-of-3 majority without a third full copy; a sound later option for
  on-prem deployments without S3 or k8s, behind the same lease interface.
- **External manager only** (operator / scripts) — fine operationally,
  but the database still needs R4's term checks, so it saves nothing.

### Milestones

- **R4** — terms, data-path fencing, semi-sync acks, diverged-tail
  rejoin, `NOT_LEADER`.
- **R5a** — lease agent with the S3 backend, self-fencing, in-process
  promotion.
- **R5b** — k8s `Lease` backend; SDKs follow leader hints.
- **R5c** — split-brain certs on the fault proxy: partition the leader,
  kill it mid-write, skew its clock; prove no acked write is lost and
  never more than one writer per term.

### Risks to settle before R5a

- **Clock drift** is the safety-critical parameter: the self-fencing
  margin must cover the worst drift between the leader and the lease
  store's clock. TTL and margin are set deliberately, never defaulted
  blindly. k8s leases compare node clocks (renewTime), so they need
  bounded drift by assumption.
- S3 conditional-write support differs across providers and
  S3-compatible stores (MinIO, R2, …) — verify per backend before
  claiming it.

## R1 known limits (shortcut comments in `db.rs`)

- The archive copy runs inside flush phase C under the WAL lock; writers
  wait one memtable's worth of WAL bytes. Move it to phase B if measured.
- `log_since` reads every archive file per call; seek by file name once
  catch-up volume matters (R3 batches calls with `max`).
- `apply_replicated` requires `DurabilityMode::Sync` on the follower.
- No crash-window cert yet for the archive → truncate window (dedup by seq
  makes it safe by construction); it lands with the R4 2-node certs.
