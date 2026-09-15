# Transaction Contract

P5-M10 (ND-10) — the public transaction API over the kernel's existing
OCC/MVCC/atomic-batch commit pipeline. Pinned by `txn_contract.rs`
(tx000–tx007).

## Isolation: SNAPSHOT only

The only isolation level is SNAPSHOT. A transaction pins the version set
visible at `begin`: every read (`Transaction::get`) resolves to the newest
committed version with `commit_ts <= snapshot_ts`, no matter what commits
afterwards.

*Honest-ledger row*: READ COMMITTED is NOT implemented. The kernel implements
snapshot semantics only; READ COMMITTED reopens on workload evidence. (The
MCP transaction tools are likewise deferred — P5-M11, where the server's
session model can own the handle registry.)

## API

- `Kernel::begin_transaction(subject, txn_id) -> Transaction` — pins the
  snapshot and registers `txn_id` for idempotent retry. Empty ids are
  rejected.
- `Transaction::stage(req)` — buffers one write. `req.context.subject` must
  match the transaction's subject. `expected_version` is pinned from the
  begin snapshot (caller-set values are overridden): a head that moved since
  begin yields the deterministic `VersionConflict { koid, expected, found }`
  at `commit` — `expected` is the version at begin, `found` the current one.
- `Transaction::get(koid)` — snapshot read, ACL-checked against the
  transaction's subject. A transaction never sees its own staged writes
  (documented limitation).
- `Transaction::commit()` — consumes the transaction; all staged writes land
  in ONE atomic engine batch together with the transaction outcome record.
- `Transaction::rollback()` — consumes the transaction; staged writes are
  discarded, storage is untouched (zero residue).

The lifecycle is enforced by construction — commit/rollback consume the
handle, so a finished transaction cannot be reused or double-finished.

## Idempotent retry

Every committed transaction writes its outcome under the reserved row
`sys/txn/<id>` in the SAME batch as its writes. A retry with the same id —
in-process or after a crash/reopen — re-reads the record, re-applies
nothing, and returns the original outcome; the journal sequence is
unchanged. The record check runs under the pipe lock, so a concurrent
same-id commit always dedupes. An empty transaction also records an empty
outcome, so a re-begin of an already-committed id dedupes identically.

## VersionConflict determinism

`stage` pins `expected_version` from the begin snapshot. A concurrent
committer that moved the head makes `commit` fail with
`VersionConflict { koid, expected: <version at begin>, found: <current> }`.
The error is deterministic, not a retryable hint — the caller retries the
whole transaction.

## Crash windows (rule 5)

Two env-armed park hooks exist on the transaction path only (the plain
remember path never parks):

- `AIKOQL_TXN_PARK=pre_commit` — after the batch is assembled (outcome
  record included), before the engine write. A kill here commits nothing.
- `AIKOQL_TXN_PARK=post_commit` — after the engine write. A kill here leaves
  the transaction durable; the retry is the recorded no-op.

Both write the `AIKOQL_TXN_PARK_MARKER` file path and sleep until killed.
Without the env var: no park, no overhead.

## Commit path is synchronous

No async suspension anywhere in `transaction/kernel.rs` or
`transaction/kernel/txn.rs` — the pipe lock is a std Mutex held across
validation and batch publication (grep-pinned by tx000).

## Metrics

`Kernel::transaction_metrics() -> TxnMetricsSnapshot`: `begun`, `committed`,
`rolled_back`, `conflicts`, `deduped_retries` — kernel-lifetime counters,
shared across clones.
