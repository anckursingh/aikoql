# HA failure matrix

**As of:** `main` at `4704d9b6` (2026-11-11). Scenarios are the
productionisation review's section 4.1, plus three this audit added.

Tests are named, not linked by line. Unless a row says otherwise they
are in `crates/services/api/mcp/tests/replication_failover.rs`
(integration, real processes); `unit:` marks a test in the `mod tests`
of the named source file under `crates/services/api/mcp/src/`.

Tests were matched to scenarios by name and by the code they exercise.
Their bodies were not all read and none was run for this document, so
"covered" means "a test exists that targets this" — the first job of
the certification work is to confirm each one asserts what its row
needs.

Coverage: **covered** — a targeted test exists. **partial** — a
neighbouring test exists but not this fault. **none** — no test found.

## 1. Matrix

| # | Failure | Expected behaviour (from code) | Existing tests | Coverage |
|---|---|---|---|---|
| 1 | Leader process killed | The lease stops changing; after a full TTL the standby wins the next term, re-executes and leads. Clients get `NOT_LEADER` hints or reconnect | `leader_dies_standby_is_promoted_with_every_object`, `ha_standby_takes_over_when_the_leader_dies`, `split_brain_a_leader_killed_mid_write_loses_no_acked_write`; `scripts/kind-failover-e2e.sh` | covered |
| 2 | Leader host lost | Same as 1 from the standby's side. The old data directory is gone, so no rejoin | as 1 (process kill only; no host or network loss) | partial |
| 3 | Leader loses the lease store but keeps its clients | The leader fences itself once 2/3 TTL passed since its last renewal started — before the standby's full TTL can lapse | `split_brain_a_leader_cut_off_from_its_lease_fences_before_the_standby_takes_over`, `ha_leader_fences_itself_when_its_lease_is_taken`, `split_brain_a_leader_whose_store_rejects_it_fences_itself` | covered |
| 4 | Standby process killed | Async: the leader keeps writing; the standby resumes or re-bootstraps on return. Sync: writes fail after `--replicate-sync-ms` | `a_sync_leader_acks_writes_only_once_a_standby_holds_them`; unit `replication.rs`: `a_sync_write_with_no_standby_fails_after_the_timeout`, `standby_streams_catches_up_and_resumes_after_disconnect` | covered |
| 5 | Replication network partition (lease store reachable from both) | The leader keeps the lease and keeps leading; the standby sees the lease renewing and does not take over. Sync writes fail; async writes accumulate lag | unit `replication.rs`: `standby_streams_catches_up_and_resumes_after_disconnect` (disconnect, not a held-open partition) | partial |
| 6 | Delayed or reordered replication messages | The stream is one ordered TCP/TLS connection; frames carry a seq | none found for out-of-order or duplicate frames | none |
| 7 | WAL gap (standby behind the retained log) | Automatic re-bootstrap from a snapshot | unit `replication.rs`: `a_standby_behind_the_retained_log_is_rebootstrapped_automatically` | covered |
| 8 | Leader crashes during a checkpoint | The standby promotes from the last shipped checkpoint and replays | `promotion_resumes_indexes_from_the_shipped_checkpoint` (promotion from a completed checkpoint; no crash mid-checkpoint) | partial |
| 9 | Standby crashes during snapshot bootstrap | `finish_rebootstrap` completes or discards an interrupted bootstrap at the next start | none found by name | none |
| 10 | Lease store or API unavailable | Leader: as 3. Standby: logs and keeps following; it cannot take over while it cannot read the lease | leader side as 3; standby side none | partial |
| 11 | Both nodes restart | The previous holder resumes its own lease and renews at once; the other follows | unit `ha.rs`: `a_lease_won_before_a_restart_is_claimed_and_led`, `resuming_a_lease_renews_it_at_once`, `another_holder_makes_this_node_a_standby`; `a_fenced_node_refuses_writes_after_a_restart` | partial — no two-process full restart with a data comparison |
| 12 | Disk full on the leader | A write that cannot be made durable must fail, not be acknowledged | none found | none |
| 13 | Disk full on the standby | Sync: the standby cannot ack, so writes fail after the timeout. Async: lag grows | none found | none |
| 14 | Certificate expiry or rotation | An untrusted or expired peer is refused; there is no plaintext fallback off loopback | unit `replication.rs`: `a_standby_that_does_not_trust_the_leader_is_refused`; `replicate_listen_rejects_a_non_loopback_bind` (trust and bind policy; no expiry, no rotation while running) | partial |
| 15 | Clock skew between nodes | No effect on election by design: each side measures elapsed time on its own monotonic clock | none targeted | none — the design claim has no test |
| 16 | Archive outage | Uploads fail and are logged; log release is held, so the leader's retained log grows. Writes continue | unit `archive.rs`: `the_archive_holds_log_release_until_it_uploads` | partial — no outage test, and nothing reports the lag |
| 17 | Promotion during an archive upload | Each term archives its own timeline; an unfinished upload is left alone by pruning | unit `archive.rs`: `each_term_archives_its_own_timeline`, `pruning_leaves_an_unfinished_upload_alone`; `real_s3_ha_failover_then_pitr_from_the_bucket` (env-gated) | partial |
| 18 | *(added)* Leader pod terminated by Kubernetes (SIGTERM) | Today: identical to a kill — no handler, no drain, no lease release; writes are unavailable for up to a full TTL plus promotion | kind e2e deletes the pod, which exercises the kill path only | partial — the graceful path does not exist |
| 19 | *(added)* Leader process paused longer than the TTL, then resumed (VM stall, SIGSTOP) | The standby has taken over. On resume the old leader's next renewal conflicts and it fences. Between resume and that renewal it may accept writes: sync mode cannot ack them (its standby is gone or on a newer term); async mode could acknowledge writes that are then discarded by re-bootstrap | unit `replication.rs`: `a_standby_refuses_an_older_term_leader_which_then_fences_itself`; `a_diverged_old_leader_rejoins_the_promoted_standby_by_itself` | partial — the window itself is untested and is a hypothesis from reading `ha.rs` `renew` |
| 20 | *(added)* Enterprise identity across promotion | See `SUPPORTED_DEPLOYMENT_MODES.md` section 2: unsupported and refused at startup (PROD-1) | `ha_an_oauth_sign_in_survives_a_failover` (community logins only); `tests/users.rs`: `multi_user_with_replication_refuses_start` | refusal covered; none for multi-user across promotion |

## 2. What the matrix says

- **Well covered:** process death, lease loss and self-fencing, sync
  acknowledgement, WAL-gap re-bootstrap. These are the scenarios the
  MRFC-0080 work was built test-first around.
- **No test at all:** disk full on either node (12, 13), frame
  ordering (6), interrupted bootstrap (9), clock skew (15).
- **Tested next door, not on the fault:** partition (5), crash during
  checkpoint (8), certificate lifetime (14), archive outage (16),
  full-cluster restart (11).
- **Row 19 needs a test before anyone reasons further about it.** If
  the async window is real it belongs in the published async contract;
  if a write-time lease-age check already closes it, the row becomes
  "covered" with that test.

## 3. Contracts to measure, not yet measured

No RPO or RTO figure exists for any mode. The expected shapes, from
configuration defaults:

| Mode | Acknowledged-write loss | Write interruption on leader loss |
|---|---|---|
| Synchronous leader/standby | none for writes acked by the standby, under one-standby acknowledgement | about one TTL (default 10 s) plus re-exec and index resume |
| Asynchronous leader/standby | whatever the standby had not received | as above |
| Archive-only | up to `--archive-log-ms` (default 5 s) | restore time: newest snapshot plus log replay; snapshot cadence default 3 600 s |

These are derivations from defaults, not results. Nothing here supports
a published zero-loss claim until rows 1, 4, 12 and 13 are asserted
under synchronous mode with the loss counted.

## 4. Proposed order for new tests

1. Row 19 (process pause) — cheapest to write, decides a contract.
2. Rows 12 and 13 (disk full) — the durability claims rest on them.
3. Row 18 (SIGTERM) — arrives with the graceful-shutdown change.
4. Row 11 (full restart) and row 5 (held partition) — integration,
   using the existing fault proxy (`crates/sdk/fault-proxy`) where it
   fits.
5. Rows 9, 14, 16, 8, 6, 15.
