# Operator runbooks

One section per alert in `deploy/prometheus/aikoql-alerts.yaml`; the
section heading is the alert name. Each gives what to look at, what is
safe to do, what can lose data, and when to stop and escalate.

Three commands answer most first questions (same port as `/metrics`):

```sh
curl -sk https://<pod>:4318/status        # role, term, lag, lease, archive, storage
curl -sk https://<pod>:4318/readyz        # 200 = serves reads; body names the role
curl -sk https://<pod>:4318/readyz/write  # 200 only on the writer
```

On Kubernetes, `kubectl get lease <leaseName> -o yaml` shows the lease
record (annotation `aikoql.io/lease`: holder, term, seq, `released`).

**Rules that hold in every section**

- Never delete or edit the lease to "unstick" a cluster while the node
  it names might still be running. The lease is what keeps two writers
  from existing.
- Never start a node with a copy of another node's data directory.
- A node that exits with code 75 fenced itself on purpose. Let its
  supervisor restart it; it comes back as a standby.
- Synchronous mode (`--replicate-sync-ms`) means an acknowledged write
  is on one standby. Asynchronous mode does not: a failover can lose the
  writes the standby had not received.

## AikoqlNoWriter

No node reports `leader` or `standalone`.

- **Diagnose:** `/status` on every node. All `standby`: the lease holder
  is gone and nobody could take over — look for "the lease lapsed but
  this standby is behind its holder" in the standby's log. All `fenced`
  or `starting`: read the last log lines before the restart.
- **Safe:** restart the node the lease names (`holder`); it resumes its
  own lease. Wait one TTL after it is back before judging.
- **Data-loss risk:** forcing a standby that is behind to lead (by
  deleting the lease) discards every write it never received. Do it only
  when the old leader's disk is gone for good, and say so in the
  incident record.
- **Escalate:** the old leader's volume is lost and the standby is
  behind — that is a restore decision (`pitr` from the archive), not a
  restart.

## AikoqlMultipleWriters

More than one node reports `leader`. Treat as split brain until shown
otherwise.

- **Diagnose:** compare `aikoql_replication_term` across nodes and the
  lease's `holder`. A stale scrape of a pod that has since exited looks
  the same — check the pod is really running.
- **Safe:** nothing needs doing if the lower-term node exits within one
  TTL: fencing is designed to stop it writing before the other may
  lead. If both keep running, stop the node that does **not** hold the
  lease.
- **Data-loss risk:** writes acknowledged by the lower-term node after
  the takeover are discarded when it rejoins (it re-bootstraps from the
  new leader). In synchronous mode it cannot acknowledge any.
- **Escalate:** always, after containment. Two live writers is a
  defect; keep both nodes' logs and the lease history.

## AikoqlLeaderChanged

The term moved: a failover, a planned stop, or a restart.

- **Diagnose:** the new leader's log has "the lease was released"
  (planned) or "the lease lapsed" (the old leader died). Find out why
  the old leader stopped.
- **Safe:** nothing by default. Confirm the old leader rejoined as a
  standby (`AikoqlStandbyDisconnected` clears).
- **Data-loss risk:** none in synchronous mode or after a planned stop.
  After a crash in asynchronous mode, compare the old leader's last seq
  (its log) with the new leader's `head_seq` at takeover.
- **Escalate:** repeated changes without a deployment — the nodes are
  flapping; see `AikoqlLeaseRenewFailing`.

## AikoqlNodeFenced

A deposed node is still serving as `fenced` instead of rejoining.

- **Diagnose:** it was restarted without its replication flags, or its
  re-bootstrap fails — read its log.
- **Safe:** restart it with the same flags as before; under `--ha-lease`
  it rejoins by itself, re-bootstrapping from the leader if its history
  diverged.
- **Data-loss risk:** its diverged writes are discarded by the
  re-bootstrap. They were never acknowledged in synchronous mode.
- **Escalate:** the re-bootstrap fails repeatedly.

## AikoqlLeaseRenewFailing

The leader's lease writes are not being answered.

- **Diagnose:** the lease store (Kubernetes API, S3 endpoint, shared
  file) from the leader's network position; credentials and RBAC.
- **Safe:** fix the store. The leader fences itself and exits once 2/3
  of a TTL passes without a renewal — that is correct, not a second
  fault.
- **Data-loss risk:** none from the failure itself.
- **Escalate:** the store is healthy and renewals still fail.

## AikoqlStandbyDisconnected

The leader has no standby following it.

- **Diagnose:** is the standby pod running; its log ("replication link
  … reconnecting", a TLS or token refusal); the network path to the
  leader's replication port.
- **Safe:** restart the standby. It resumes from its last seq, or
  re-bootstraps if the leader no longer holds the log it needs.
- **Data-loss risk:** none now — but there is no failover target, and
  in synchronous mode writes fail until a standby returns.
- **Escalate:** the standby cannot re-bootstrap.

## AikoqlReplicationLagHigh

A standby is connected but far behind.

- **Diagnose:** the standby's disk and CPU; a burst of writes on the
  leader; the link.
- **Safe:** wait if it is closing the gap (`lag_frames` in `/status`
  falling). Otherwise restart the standby.
- **Data-loss risk:** in asynchronous mode the lag is the loss window
  if the leader dies now.
- **Escalate:** the lag grows steadily under normal load — the standby
  is undersized.

## AikoqlArchiveFailing

Archive passes, snapshots or prunes are returning errors.

- **Diagnose:** the leader's log lines starting `archive:`; the bucket
  or directory, its credentials, its free space.
- **Safe:** fix the archive. Nothing is lost while it is down: the
  database keeps writing and holds the log the archive has not taken.
- **Data-loss risk:** none directly — but the held log grows on the
  leader's volume (`AikoqlDiskSpaceLow` is the next alert), and the
  archive's recovery point is frozen at its last success.
- **Escalate:** the volume is filling faster than the archive can be
  repaired.

## AikoqlArchiveStale

No archive pass has succeeded recently.

- **Diagnose:** as `AikoqlArchiveFailing`; also whether the archive
  loop is running at all (the series is absent if `--archive` was
  dropped from the command line).
- **Safe:** as `AikoqlArchiveFailing`.
- **Data-loss risk:** a total loss of both nodes now loses everything
  since `last_success_unix_ms`.
- **Escalate:** immediately if a standby is also down — the archive is
  then the only other copy.

## AikoqlArchiveLagHigh

The archive is succeeding but far behind the head.

- **Diagnose:** upload throughput to the object store; an unusually
  large write burst.
- **Safe:** wait if the lag is falling.
- **Data-loss risk:** the lag is the archive-only loss window.
- **Escalate:** the lag does not fall under normal load.

## AikoqlSnapshotOverdue

No archive snapshot for over 26 hours.

- **Diagnose:** a snapshot is skipped while the database is unchanged,
  so an idle database is expected to trip this — check `head_seq` has
  moved. Otherwise read the `archive:` log lines.
- **Safe:** none needed for an idle database. For a busy one, fix the
  archive as in `AikoqlArchiveFailing`.
- **Data-loss risk:** none — but restore time grows with the log
  replayed since the last snapshot.
- **Escalate:** snapshots fail while log uploads succeed.

## AikoqlRestoreDrillFailed

The scheduled `restore-drill` exited non-zero.

- **Diagnose:** the drill's JSON report: the first check with
  `"ok": false` and its `detail`. `restore` failed: the archive is
  incomplete or unreadable. `expected seq`: the archive is behind what
  you require. `open` or `health`: the restored copy is damaged.
- **Safe:** re-run the drill by hand to rule out a scratch-volume or
  credentials problem; it only reads the archive and writes a scratch
  directory it removes.
- **Data-loss risk:** none from the drill. The finding is that a real
  restore might not work.
- **Escalate:** any `restore`, `open` or `health` failure that
  reproduces. Until it is resolved the archive is not a recovery path;
  keep both database nodes healthy and do not prune the archive.

A drill as a Kubernetes CronJob (give it read access to the archive and
an `emptyDir` large enough for one restored copy):

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: aikoql-restore-drill
spec:
  schedule: "17 3 * * *"
  concurrencyPolicy: Forbid
  jobTemplate:
    spec:
      backoffLimit: 0
      template:
        spec:
          restartPolicy: Never
          containers:
            - name: drill
              image: aikoql:0.4.0
              args: ["restore-drill", "--archive", "s3://<bucket>/<prefix>",
                     "--scratch-root", "/scratch"]
              volumeMounts:
                - {name: scratch, mountPath: /scratch}
          volumes:
            - name: scratch
              emptyDir: {}
```

## AikoqlDiskSpaceLow

A data volume is nearly full.

- **Diagnose:** `storage` in `/status` (WAL bytes, segment bytes,
  compaction backlog); whether the archive or a standby is holding log
  back (their `lag_frames`).
- **Safe:** grow the volume. Restore a stalled archive or standby so
  the held log is released.
- **Data-loss risk:** a write that cannot be made durable is refused,
  not acknowledged — a full disk loses availability, not acknowledged
  data. Do not delete files from the data directory by hand.
- **Escalate:** the volume cannot be grown and is still filling.

## AikoqlCompactionBacklog

A large amount of data is waiting for compaction.

- **Diagnose:** sustained write load above what compaction keeps up
  with; disk throughput.
- **Safe:** reduce write load, or give the node more I/O. Writers are
  slowed on purpose before the backlog grows without bound.
- **Data-loss risk:** none.
- **Escalate:** the backlog grows with no write load.

## AikoqlMetricsError

A scrape could not read the store.

- **Diagnose:** the node's log lines starting `metrics:`.
- **Safe:** none needed for a single occurrence during a restart.
- **Data-loss risk:** none from the scrape; the read failure behind it
  may matter.
- **Escalate:** it persists — the store is returning errors to readers
  too.

## Not covered

No alert exists for these yet, because the server exports no series for
them. Watch them by other means until it does.

- **Certificate expiry** (replication and serving certificates): track
  the Secret's `notAfter` with your certificate tooling.
- **KMS availability**: a node that cannot reach its KMS fails at
  startup; nothing reports a KMS outage while it runs.
- **Synchronous writes timing out**: visible to clients as write
  errors and in the leader's log, not as a counter.
- **OIDC provider outage**: sign-ins fail; existing sessions and the
  break-glass admin keep working.
