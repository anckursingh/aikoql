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
| `NOT_LEADER` + leader hint | R4d ✅ |
| automatic failover: lease agent (file + S3), self-fencing, in-process promotion | R5a ✅ |
| k8s `Lease` backend, SDKs follow leader hints | R5b ✅ (Java SDK pending) |
| standby serving reads (a read-only kernel over a replicating db) | later |
| split-brain certs on the fault proxy | R5c |
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

## R4d — `NOT_LEADER` ✅

- A write to a fenced node fails with envelope code **`NOT_LEADER`**
  (`retryable: true`) and, when known, `leader`: the newer leader's
  client address (`docs/error-codes.md`).
- The hint is `<dir>/REPL-LEADER` — the newest leader's client address
  this node knows. A leader records its own from `serve --advertise
  HOST:PORT`; the `ok` reply hands it to standbys (kept across a
  re-bootstrap); a standby's hello hands it to a leader it deposes,
  whose fence reason then reads `… (leader=ADDR)`. Promotion deletes a
  stale hint — a new leader never serves its old leader's address.
- Trust boundary: an address off the wire must be a bounded host:port
  token (`valid_addr`), since it is echoed to clients.

Tests: `error_codes.rs` (classification + `leader` field),
`replication.rs` (the hint travels leader → standby → deposed leader;
promotion clears it), `replication_failover.rs` (a fenced restart
answers NOT_LEADER + leader; `--advertise` is recorded). RED:
`r4d-not-leader{,-e2e}`.

R4d limits:
- Only fenced nodes answer NOT_LEADER: a standby runs `follow`, not
  `serve`, so it takes no client traffic until R5a's in-process standby.
- Clients must follow the hint themselves until R5b teaches the SDKs.
- The hint is as fresh as the last handshake that carried it; R5's
  lease becomes the authoritative source.

R4 is complete: terms + fencing (R4a), RPO = 0 for acked writes (R4b),
self-healing rejoin (R4c), client redirection (R4d). R5a builds on all
four.

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
- **R5b** — k8s `Lease` backend; SDKs follow leader hints. ✅
- **R5c** — split-brain certs on the fault proxy: partition the leader,
  kill it mid-write, skew its clock; prove no acked write is lost and
  never more than one writer per term.

### Risks to settle before R5a (status after R5a: clock drift designed out — see R5a; S3-compatible stores still unverified)

- **Clock drift** is the safety-critical parameter: the self-fencing
  margin must cover the worst drift between the leader and the lease
  store's clock. TTL and margin are set deliberately, never defaulted
  blindly. k8s leases compare node clocks (renewTime), so they need
  bounded drift by assumption.
- S3 conditional-write support differs across providers and
  S3-compatible stores (MinIO, R2, …) — verify per backend before
  claiming it.

## R5a — automatic failover ✅

`serve --ha-lease <PATH | s3://BUCKET/KEY> --ha-node ID [--ha-ttl-ms MS]
--replicate-listen ADDR --tcp-token TOKEN::admin [--advertise HOST:PORT]`
on every node; each node runs the `ha` agent (`crates/services/api/mcp/src/ha.rs`).

- **Lease record** `{holder, term, repl, client, seq, beat}` in a
  `LeaseStore` whose only write is a CAS on a version. `FileLease` (one
  host, tests): content hash under an OS lock. `S3Lease` (R5a-2): the
  ETag; create = `PUT If-None-Match: *`, renew / take over = `PUT
  If-Match: <etag>`; 412 and 409 are a lost CAS; SigV4 with the standard
  `AWS_*` variables, `AIKOQL_S3_ENDPOINT` for S3-compatible stores.
- **Standby phase** (before the kernel opens): follow the holder's
  `repl` in-process (`follow_loop`, shared with `follow`, R4c rejoin
  included). Take over only when the record has stayed **unchanged for a
  full TTL on this node's monotonic clock** and this node holds at least
  the holder's last renewed `seq`: CAS to `term + 1`, `claim_term`, fall
  through into the normal serve path — the in-process promotion.
- **Leader**: the renewer starts at the win (before the kernel opens, so
  a slow open cannot let the lease lapse) and renews every TTL/3. A lost
  CAS, or 2/3 TTL since the last renewal *started* without success →
  fence the Db, record the new holder's client address (the NOT_LEADER
  hint) and `fenced`, **exit 75**; the supervisor restarts the node into
  the standby phase. A leader that restarts while its lease is live
  resumes it.
- **No cross-node clock comparison**: a renewal lands after it starts
  and is seen after it lands, so the old leader stops writing at least
  TTL/3 before a standby may take over; the margin covers clock-*rate*
  drift and in-flight writes. (The risk noted below for wall-clock
  expiry does not arise.)

Measured (`replication_failover.rs`): **failover 1.77 s at TTL 1.5 s**
(leader crash → the standby answers its first request as leader).

Tests: `ha.rs` — the CAS contract for both stores (one winner among
concurrent creators), SigV4 against AWS's documented GET Object
signature, UTC date formatting, `s3://` parsing; `replication_failover.rs`
— the leader dies and the standby takes over with term 2 holding every
object; a leader whose lease is taken self-fences (exit 75, `fenced`,
hint recorded). RED: `r5a-lease-agent{,-e2e}`, `r5a-s3-lease`.

R5a limits:
- **S3 conditional writes are verified against a fake only.** AWS S3
  documents both conditions on PUT; MinIO, R2 and other S3-compatible
  stores must each be checked before claiming support (open risk).
- Replication is still loopback-only (TLS is R3c), so a real multi-host
  HA deployment waits for it; today's HA runs on one host.
- A standby takes no client traffic before promotion (stdio and TCP
  answer nothing until it leads); standby reads and NOT_LEADER from
  standbys are R5b.
- Eligibility uses the holder's seq **at its last renewal**: with
  semi-sync and several standbys, a standby that missed acked writes
  after that renewal could still win. One standby, or a quorum count,
  closes it.
- The node must run under a supervisor (systemd, k8s) — a self-fenced
  leader exits rather than demote in place.

## R5b — Kubernetes lease, SDKs follow the leader ✅

**R5b-1 — `--ha-lease k8s://NAMESPACE/NAME`.** The lease as a
`coordination.k8s.io/v1` Lease: the version is `metadata.resourceVersion`;
create is POST (409 AlreadyExists = a lost CAS), renew / take over is PUT
carrying that resourceVersion (409 when it moved). The record rides the
`aikoql.io/lease` annotation; `spec.holderIdentity` names the holder for
`kubectl get lease`; a Lease without the annotation is refused, never
adopted. In a pod: the service-account token (Bearer) and CA (rustls root
store — rustls 0.23 + ring already in the tree, default features off so
aws-lc-rs stays out), the API server from `KUBERNETES_SERVICE_HOST/PORT`,
a TTL/3 request timeout. RBAC: the pod's service account needs `get`,
`create`, `update` on `leases` in that namespace.

**R5b-2 — SDKs follow `NOT_LEADER`.** Go, Rust, Python, TypeScript: a TCP
client whose tool call is refused with `NOT_LEADER` and a `leader` hint
reconnects to that address with the same token and client identity,
re-runs `initialize` and retries the call **once**; it then stays on the
leader. No hint → the retryable `NOT_LEADER` is returned. The refused
call was never applied (the fence rejects before the WAL), so the retry
cannot double-write.

Tests: `ha.rs` — the CAS contract against a fake Kubernetes API (bearer
auth, holderIdentity), `k8s://` parsing; per SDK, two scripted fakes (a
fenced one answering NOT_LEADER + hint, the leader) — the leader sees
`initialize, tools/call, tools/call`; no hint → NOT_LEADER returned.
RED: `r5b-k8s-lease`, `r5b-sdk-hint-{go,rust,python,ts}`.

R5b limits:
- **Java SDK not done**: no JDK on the build machine; the same change is
  pending there.
- The Kubernetes store is verified against a fake API only; the
  in-cluster TLS/token path has not run in a real cluster.
- `session_init` identity (run_id) is not replayed on the new
  connection; stdio and native-protocol clients do not follow hints.
- The client trusts the hint its server sends (the token goes to the
  named leader). The hint comes from the lease / replication handshake
  and is validated as a bounded host:port token, but a client that must
  pin its endpoints should not use a fenced-node address as a seed.
- Standby reads are not in R5b: a standby still answers no client
  traffic until it leads. It needs a kernel that opens without writing
  over a db being replicated into — a separate milestone.

## R1 known limits (shortcut comments in `db.rs`)

- The archive copy runs inside flush phase C under the WAL lock; writers
  wait one memtable's worth of WAL bytes. Move it to phase B if measured.
- `log_since` reads every archive file per call; seek by file name once
  catch-up volume matters (R3 batches calls with `max`).
- `apply_replicated` requires `DurabilityMode::Sync` on the follower.
- No crash-window cert yet for the archive → truncate window (dedup by seq
  makes it safe by construction); it lands with the R4 2-node certs.
