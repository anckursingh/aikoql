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
| TLS for cross-host links; lag metrics endpoint | R3c ✅ |
| terms + fencing in the data path | R4a ✅ |
| semi-sync ack (RPO = 0 for acked writes) | R4b ✅ |
| diverged-tail rejoin (and automatic re-bootstrap) | R4c ✅ |
| `NOT_LEADER` + leader hint | R4d ✅ |
| automatic failover: lease agent (file + S3), self-fencing, in-process promotion | R5a ✅ |
| k8s `Lease` backend, SDKs follow leader hints | R5b ✅ |
| standby serving reads: read replicas (`serve --follow`) and HA standbys | ✅ |
| split-brain certs: partition, kill mid-write, rejected store | R5c ✅ |
| log + snapshot archive to S3 (PITR) — the enterprise S3 leg | R6 ✅ |
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
tokens — the `--listen` rules. Cross-host links: TLS since R3c (below).

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

## R3c — TLS between hosts, lag metrics ✅

**R3c-1 — TLS.** Every replication connection is a `Link`: plaintext or
rustls (ring provider, the crate's only TLS stack — shared with the k8s
lease via the `tls` module).
- Leader: `--replicate-tls-cert PEM --replicate-tls-key PEM` (both or
  neither). With them the listener may bind any interface; without,
  `--replicate-listen` stays loopback-only (plaintext tokens).
- Standby: `follow --tls-ca PEM` / `serve --replicate-tls-ca PEM` (HA)
  trust exactly that CA; the leader's certificate must carry the host
  the standby dials (IP or DNS SAN). `--replicate-advertise HOST:PORT`
  is the address recorded in the HA lease when the listen address
  (e.g. `0.0.0.0:7071`) is not dialable.
- A TLS failure (untrusted certificate, protocol error — rustls's
  InvalidData) is fatal for `follow`, never retried.
- Server auth only: standbys are authenticated by their admin token
  inside the encrypted stream; mTLS is a later option.
- One owner per connection: a rustls stream cannot be split across a
  reader and a writer thread, so the leader now reads acks on its
  sending thread (`poll_acks`: 1 ms between batches; 10 ms when idle,
  which doubles as the head poll — the R4b ack latency is unchanged).

**R3c-2 — lag metrics.** A serving leader's `/metrics` scrape
(`--metrics-addr`) carries:

```
aikoql_replication_head_seq            the leader's newest committed seq
aikoql_replication_standbys            connected standbys
aikoql_replication_standby_lag_frames{standby="ID",peer="ADDR"}  head − acked
```

`Acked` holds each connected standby's (peer, acked seq); the slowest
still bounds log release.

Tests: `replication.rs` — a TLS standby streams across flushes; one
trusting an unrelated CA is refused with a certificate error and applies
nothing; the lag series drains to 0 and the standby count follows
connects/disconnects. `replication_failover.rs` — a TLS leader on
`0.0.0.0` is followed via `follow --tls-ca` and promoted with every
object; the leader's real `/metrics` scrape carries the series. Fixtures:
`tests/fixtures/tls/` (test-only P-256 certs, CA keys deleted). RED:
`r3c-tls{,-e2e}`, `r3c-lag-metrics{,-e2e}`.

Harness fix found on the way: `free_port` handed out OS-ephemeral ports,
which a client connection's local end could take before the server bound
them (`Address already in use`, ~1 run in 10 as the suite grew). Ports
now come from below the ephemeral ranges; an unlogged server's stderr is
kept and printed when it exits.

R3c limits:
- Exercised on one machine (loopback + `0.0.0.0` bind); no cross-host
  run yet.
- No certificate reload: rotating the leader's cert needs a restart.
- Standby-side lag (as seen by the standby) is not exported — a
  standby runs `follow`, which serves no metrics; the leader's per-standby
  series covers it while the standby is connected.

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
  never more than one writer per term. ✅ (see R5c below)

### Risks to settle before R5a (status: clock drift designed out — see R5a; MinIO verified, AWS S3 / R2 still unverified — see "Real-S3 certs")

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
- **S3 conditional writes: verified on MinIO** (real-S3 certs below);
  AWS S3 documents both conditions on PUT but has not been run here, and
  R2 and other S3-compatible stores must each still be checked.
- ~~Replication is loopback-only~~ — R3c added TLS: HA nodes on
  different hosts use `--replicate-tls-cert/-key/-ca` and
  `--replicate-advertise`. (Not yet exercised across real hosts.)
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

**R5b-2 — SDKs follow `NOT_LEADER`.** Go, Rust, Python, TypeScript, Java: a TCP
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
- ~~Java SDK not done~~ — done (`LeaderHintTest`; RED `r5b-sdk-hint-java`).
- ~~The Kubernetes store is verified against a fake API only~~ — certified
  on a real kube-apiserver (envtest), including the in-cluster path; not
  yet run inside a real cluster's pod (see "Kubernetes lease certs").
- `session_init` identity (run_id) is not replayed on the new
  connection; stdio and native-protocol clients do not follow hints.
- The client trusts the hint its server sends (the token goes to the
  named leader). The hint comes from the lease / replication handshake
  and is validated as a bounded host:port token, but a client that must
  pin its endpoints should not use a fenced-node address as a seed.
- Standby reads are not in R5b: a standby still answers no client
  traffic until it leads. It needs a kernel that opens without writing
  over a db being replicated into — a separate milestone.

## R5c — split-brain certs ✅

Harness (`replication_failover.rs`): a fake S3 with conditional PUT
holds the lease; a **Gate** in front of one node's S3 endpoint is open,
**blackholes** it (connections hang — a partition) or **rejects** it
(403 `RequestTimeTooSkewed`). The SDK fault proxy works on one client's
wire and cannot cut a node from its lease store, hence the Gate.

**One writer per term — proven, not timed.** Both nodes log to one
machine's clock. The old leader logs `fenced; exiting` *after* its Db
fence took effect; the standby logs `the lease lapsed — taking over`
*before* its CAS (added for this cert). `fenced < taking over` on the
log timestamps therefore orders the real events: the old leader stopped
writing before the new one could start.

| cert | fault | proves |
|---|---|---|
| partition | the leader's lease store blackholed, writes continue | it fences (exit 75, `1 fenced`) **before** the standby takes over (`2 leader`); every write acked before or during the partition is on the new leader |
| kill mid-write | SIGKILL with a write in flight (sync acks) | every acked write is on the promoted standby |
| rejected store | the store answers 403 (a skewed SigV4 clock) | the leader fences itself — fail-safe |

Measured (TTL 1.5 s): the old leader fenced **~0.79 s** before the
standby began taking over, every run — the designed ≥ TTL/3 margin; 54–64
writes acked across the partition, none lost.

Found and fixed: `fence()` read the lease for the NOT_LEADER hint before
recording `fenced`; under a partition that read waits out its timeout
(TTL/3), delaying the record and the exit by ~0.5 s. The hint is now
read last, best-effort.

**Clock skew.** The agent's timing reads only monotonic clocks — no
wall-clock comparison between nodes — so wall-clock skew cannot open a
split brain; it can only make a store reject the leader (SigV4), which
fences it (the third cert). Clock-*rate* drift is what the TTL/3 margin
covers: the leader may run up to a third slow before the bound breaks.

RED: `r5c-split-brain` (no "taking over" line to order against).

R5c limits:
- Single host: the certs partition the lease path, not a real network
  between hosts (R3c's TLS makes multi-host possible; the certs still
  run on one machine).
- A partition of the replication link alone is not certified: takeover
  depends only on the lease, so a standby cut off from the leader's
  frames but not from the store never promotes — by construction, not
  by a cert.
- Clock-rate drift is argued, not injected.

## R6 — continuous archive, point-in-time restore ✅

`serve --archive DIR|s3://BUCKET[/PREFIX] [--archive-log-ms 5000]
[--archive-snapshot-secs 3600]` on the leader; restore anywhere with
`aikoql-mcp pitr --archive URL --to DIR [--until-seq N | --until-ms
UNIX_MS]`, then `serve DIR`.

**Layout** (`archive.rs`), one directory per replication term:

```
term-{T:010}/snap/{seq:020}-{ms:013}/<snapshot files>   COMPLETE written last
term-{T:010}/log/{first:020}-{last:020}-{ms:013}.frames  encoded WAL frames
```

- **Timelines never mix.** A new leader (a new term) starts its own
  directory with a fresh snapshot, so a diverged old leader's archived
  tail can never be replayed into the new history — the R4c argument,
  applied to the archive.
- **Archiver** (on the serving leader; a fenced node does not archive):
  resumes its term's newest object or starts with a snapshot; every
  `--archive-log-ms` uploads the retained log past what it holds, in
  objects of ≤ 4096 frames; every `--archive-snapshot-secs` uploads a
  snapshot (bounding replay). It **holds log release** as an entry in
  `Acked` — frames are never dropped before they are uploaded, with or
  without standbys — and appears in the lag series as `peer="archive"`.
  A log that no longer holds its position (e.g. a restart after a
  release) restarts the timeline from a snapshot.
- **Restore** picks the newest timeline with a complete snapshot at or
  before the target, restores it (`restore_from`, verified) and replays
  frames with `apply_replicated` up to the target seq — or, for
  `--until-ms`, every log object uploaded by that instant. A missing
  object fails the restore rather than stopping short.
- Storage: `SnapshotInfo.last_seq` — the exact seq a snapshot holds, read
  inside its capture hold.
- Stores: a directory (local disk, NFS; keys validated as plain relative
  paths) or S3 (`ListObjectsV2` paginated, SigV4 via the shared `s3`
  client the lease now uses too; `AWS_*` env, `AIKOQL_S3_ENDPOINT`).

**RPO / RTO.** Archive RPO = one log period (5 s default) for an
async leader. Restore time = snapshot download + replay since that
snapshot (bounded by the snapshot period).

Tests: `archive.rs` — the store contract on a directory and on an S3
fake (prefix-scoped, ListObjectsV2), list-XML parsing; restore to the
latest and to a mid seq byte-equal; restore until an instant gets exactly
what was archived by then; each term archives its own timeline and
restore picks the right one; the archive holds log release.
`replication_bootstrap.rs` — `last_seq` is exact. `replication_failover.rs`
— a leader archives two bursts and dies; `pitr` restores all of it, and
`--until-ms` between the bursts holds only the first. RED: `r6-*`.

R6 limits:
- **Verified on MinIO, not on AWS S3** (real-S3 certs below; the SigV4
  signer is checked against AWS's documented example).
- Time granularity is one log batch: `--until-ms` restores what had
  been *uploaded* by then, not each write's commit time (frames carry no
  wall-clock stamp).
- ~~No archive retention~~ — `--archive-keep N` / `archive-prune` (see
  "Archive retention" below).
- Snapshot files are uploaded whole, one in memory at a time (shortcut
  comment) — multipart streaming once single segments outgrow memory.
- A restored directory is a standalone node (term 0, no history); with
  field encryption the KMS key file must be restored beside it (R2 note).
- Restoring to a seq in an older timeline after a newer one began
  returns that older history, including any tail the old leader wrote
  after it was deposed (never acked under `--replicate-sync-ms`).

## Standby reads — read replicas ✅

`serve --follow LEADER_REPL_ADDR [--replicate-tls-ca CA.pem] DIR` (token:
`AIKOQL_REPLICATION_TOKEN` or an admin `--tcp-token`) serves MCP reads
from a directory it keeps replicating into; writes answer `NOT_LEADER`
with the leader's client address.

- **Kernel** (`Kernel::open_standby`): opens without writing a row — no
  type-index backfill, job recovery reads without marking, the catalog
  is checked (never bootstrapped), index declarations load without
  recording state, subscriptions are in-memory only.
- **`Kernel::catch_up`** (every 100 ms): reload what open caches (journal
  head, auth, schemas), **advance the HLC** to the caught-up commit, and
  broadcast the new journal events once, so the index maintainer follows
  as if they were local commits.
- **Storage**: the replica's store is fenced before the kernel opens, so
  any client write — or a stray open-time one — fails `NOT_LEADER`,
  while `apply_replicated` (now on `StorageAdminApi`) keeps applying.
  The fence reason is replaceable, so the hint follows the leader.
- **Re-bootstrap**: a snapshot cannot be swapped under a live kernel. The
  replica records it in `<dir>.rejoin.json` and exits 75; the restart
  finishes the swap (`finish_rebootstrap`) before opening.

Found on the way: reads snapshot at the kernel's clock (`hlc.current`),
which nothing advanced on a replica — `get` and type scans (which read
heads) converged, text search saw nothing newer than the replica's open.
`catch_up` now observes the head's commit timestamp
(RED `standby-snapshot-clock`).

Consistency: eventual, bounded by apply lag + the 100 ms catch-up period
(no read-your-writes across leader → replica).

Tests: `kernel/tests/standby.rs` (open/subscribe/catch_up write nothing;
catch_up advances head, clock and broadcasts once); `replication_failover.rs`
— a replica serves `get`, `aikoql` and `find_similar` that converge on
the leader's writes, and refuses a write with `NOT_LEADER` + hint. RED:
`standby-kernel`, `standby-reads-e2e`, `standby-snapshot-clock`.

### HA standbys serve reads ✅

An HA node that does not hold the lease now serves exactly like a
`--follow` replica — reads, `NOT_LEADER` with the holder's address, no
stream, no archive — while `ha::stand_by` follows the holder into its
open engine (shipped index checkpoints kept, so promotion stays fast).

- **Promotion by re-exec.** A standby kernel and its listeners cannot be
  swapped for a leader's in place, so the winner re-executes itself
  (unix `exec`: same pid and stdio; elsewhere it exits 75 for its
  supervisor). The fresh process meets its own lease in `ha::begin` — at
  a newer term than its directory — claims the directory and leads. A
  stdio client keeps its pipe but must re-`initialize`; a request in
  flight at the exec is lost (TCP clients reconnect).
- **`ha::begin`** decides at start-up: no lease → win it; own lease →
  claim if needed and **resume with an immediate CAS renewal**; another
  holder → standby.
- **Found and fixed (R5a):** resuming a lease after a restart started the
  fence timer at the restart instead of at a renewal that landed. A
  leader restarting late in its lease could write while a standby took
  the lapsed lease over — a split-brain window up to TTL/3. The resume
  renewal closes it (`ha.rs`: `resuming_a_lease_renews_it_at_once`).

Measured: failover 2.6 s at TTL 3 s, including the re-exec.

Limits:
- An index declared on the leader reaches a replica at its restart
  (`catch_up` does not reload declarations — it would drop the indexes
  the maintainer fills).
- An encrypted replica needs an initialized directory (field encryption
  writes its crypto-metadata row on first open, which the fence refuses).
- ~~No cascading~~ — a `--follow` replica may also `--replicate-listen`
  (below).

### Cascading replicas ✅

`serve --follow UPSTREAM --replicate-listen ADDR …` serves the stream on
to further replicas: leader → R1 → R2. R1 retains its log and serves
frames, snapshots, its term history and the leader's client address from
its own directory — R2 re-bootstraps from R1, judges divergence against
the same timeline, and its NOT_LEADER names the real leader. HA standbys
do not serve the stream (the lease names only the leader). Test:
`replication_failover.rs` — a write on the leader reaches R2 through R1;
R2's hint is the leader's. RED: `cascade-e2e`.

Limits: each hop adds its own apply + catch-up delay; R1's acks release
R1's log only (the leader's release still follows its direct standbys);
`--replicate-sync-ms` counts direct standbys only.

## Real-S3 certs (MinIO, Moto) ✅

The S3 paths were verified against fakes only; the same contracts now
run on a real S3-compatible server — `scripts/s3-it.sh` (a throwaway
MinIO; `brew install minio minio-mc`), or any store via
`AIKOQL_TEST_S3_ENDPOINT` + `AIKOQL_TEST_S3_BUCKET` + `AWS_*`. Without
them the certs skip, so the default suite is unchanged.

| cert | proves on the real store |
|---|---|
| `ha.rs` `real_s3_lease_is_a_compare_and_swap` | `If-None-Match: *` creates once, a stale `If-Match` is refused, a renewal changes the ETag; 8 concurrent creators → exactly one winner |
| `archive.rs` `real_s3_store_keeps_keys` | put/get/overwrite, a missing key is an error, `ListObjectsV2` lists by prefix |
| `archive.rs` `real_s3_archive_round_trips` | base snapshot + log batch up, listed back, restored byte-equal |
| `replication_failover.rs` `real_s3_ha_failover_then_pitr_from_the_bucket` | lease + archive in the bucket: the leader dies, the standby takes the lease (term 2) and archives its own timeline, `pitr` restores term 2 with every object |

Green on two independent S3 implementations: MinIO
RELEASE.2025-10-15T17-29-55Z and Moto 5.2.5 (`S3_IT_BACKEND=moto`, or
`"minio moto"` for both). A signed probe confirms Moto enforces the
conditional writes (412 on a repeated `If-None-Match: *` and on a stale
`If-Match`), so it is a valid lease target — an emulator that ignored
them would fail the lease cert. CI: `s3-compat-moto` (pinned, no Docker)
gates; `s3-compat` (MinIO containers) is `continue-on-error` until its
images are pinned.

**AWS:** the virtual-hosted request targets AWS uses are pinned by unit
tests (`s3.rs`), and the same certs run on AWS itself with
`S3_IT_BACKEND=aws AIKOQL_TEST_S3_BUCKET=… AWS_ACCESS_KEY_ID=…
AWS_SECRET_ACCESS_KEY=… AWS_REGION=… scripts/s3-it.sh` (no endpoint —
virtual-hosted; the script refuses without explicit credentials, bucket
and region; use a throwaway bucket and short-lived, bucket-scoped
credentials — objects stay under `it-*`). **Not yet run on AWS.** R2:
not a target until someone needs it (same script, its endpoint).

## Archive retention ✅

`serve --archive … --archive-keep N` prunes after each periodic
snapshot; `aikoql-mcp archive-prune --archive URL --keep N` prunes on
demand (`archive::prune`).

- **Kept:** the newest N complete snapshots across terms, ordered by
  (term, seq, ms), and every log object after the oldest kept snapshot
  in its term.
- **Deleted:** older snapshots in a kept term (by (seq, ms) — an idle db
  snapshots the same seq again), log objects whose last seq the oldest
  kept snapshot covers, and every object of an older term with no kept
  snapshot.
- **Never touched:** an upload in flight — a snapshot without COMPLETE
  at or after its term's oldest kept one, or a newer term whose first
  snapshot is still uploading.
- A restore target older than the oldest kept snapshot is gone: that is
  the retention boundary (`--archive-keep × --archive-snapshot-secs` of
  history).
- The archiver no longer re-snapshots an unchanged db every period.

Tests: `archive.rs` (two terms → the older one goes, the newest
restores, an early target no longer does; keep 1; idempotent; a
same-seq tie; an in-flight upload survives; on MinIO via
`scripts/s3-it.sh`), `replication_failover.rs` (`--archive-keep 2` with
1 s snapshots keeps ≤ 2; `archive-prune --keep 1`; pitr restores every
object). RED: `archive-retention{,-e2e}`.

## Kubernetes lease certs (real kube-apiserver) ✅

`scripts/k8s-it.sh` runs envtest's **kube-apiserver + etcd** natively on
loopback — no cluster, no Docker, no VM (~11 s a run, a few hundred MB) —
with a throwaway CA, serving certificate, `system:masters` static token
and service-account signing key, then the `real_k8s` certs (skipped
unless `AIKOQL_TEST_K8S_API` / `_TOKEN` / `_CA` are set):

| cert | proves on the real apiserver |
|---|---|
| `ha.rs` `real_k8s_lease_is_a_compare_and_swap` | POST creates once (409 AlreadyExists), a stale `resourceVersion` is refused (409 Conflict), a renewal changes it; 8 concurrent creators → exactly one winner |
| `replication_failover.rs` `real_k8s_ha_failover_on_a_lease` | the binary's in-cluster path (service-account token + CA, `KUBERNETES_SERVICE_*`) on `k8s://default/<name>`: the leader dies, the standby takes the Lease at term 2, every object serves, and the Lease's `spec.holderIdentity` names the new holder |

`AIKOQL_K8S_SA_DIR` points the in-cluster path at a token + `ca.crt`
outside a pod (default: the pod mount). Green on kube-apiserver v1.37.0
(`ENVTEST_K8S_VERSION` pins it). CI: `k8s-lease`, `continue-on-error`
until its first pushed run is green. Not covered: a real cluster's pod
(service-account projection, RBAC scoped to `leases` — the certs run as
`system:masters`). RED: `k8s-lease-e2e`.

## R1 known limits (shortcut comments in `db.rs`)

- The archive copy runs inside flush phase C under the WAL lock; writers
  wait one memtable's worth of WAL bytes. Move it to phase B if measured.
- `log_since` reads every archive file per call; seek by file name once
  catch-up volume matters (R3 batches calls with `max`).
- `apply_replicated` requires `DurabilityMode::Sync` on the follower.
- No crash-window cert yet for the archive → truncate window (dedup by seq
  makes it safe by construction); it lands with the R4 2-node certs.
