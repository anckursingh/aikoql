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
| 6 | Delayed or reordered replication messages | The stream is one ordered TCP/TLS connection. At the engine, a frame delivered again is a no-op and a frame that skips a seq is refused — a follower never holds a hole | storage `tests/replication_log.rs`: `redelivery_is_a_no_op_and_a_gap_fails_closed` (found after the first pass of this audit) | covered at the engine; no wire-level reorder test |
| 7 | WAL gap (standby behind the retained log) | Automatic re-bootstrap from a snapshot | unit `replication.rs`: `a_standby_behind_the_retained_log_is_rebootstrapped_automatically` | covered |
| 8 | Leader crashes during a checkpoint | The standby promotes from the last shipped checkpoint and replays | `promotion_resumes_indexes_from_the_shipped_checkpoint` (promotion from a completed checkpoint; no crash mid-checkpoint) | partial |
| 9 | Standby crashes during snapshot bootstrap | `finish_rebootstrap` completes or discards an interrupted bootstrap at the next start | none found by name | none |
| 10 | Lease store or API unavailable | Leader: as 3. Standby: logs and keeps following; it cannot take over while it cannot read the lease | leader side as 3; standby side none | partial |
| 11 | Both nodes restart | The previous holder resumes its own lease at the same term and renews at once; the other follows again | `ha_both_nodes_restart_and_the_holder_resumes_with_every_object` (PROD-6); unit `ha.rs`: `a_lease_won_before_a_restart_is_claimed_and_led`, `resuming_a_lease_renews_it_at_once`, `another_holder_makes_this_node_a_standby`; `a_fenced_node_refuses_writes_after_a_restart` | covered for the holder-first start order; the standby-first order (it takes over after a TTL) is not tested |
| 12 | Disk full on the leader | A write whose WAL append fails is refused, takes no seq and leaves no partial frame. **PROD-6 fixed a bug here:** the seq was consumed before the append, so one failed write left a hole in the replication log and `last_seq` named a write that did not exist | storage `tests/replication_log.rs`: `a_failed_wal_append_takes_no_seq_and_leaves_no_hole_in_the_log`; `tests/group_commit.rs`: `a_failed_group_append_takes_no_seq_and_the_next_group_is_contiguous`; an fsync error fences the engine in both paths (`replication_log.rs`: `a_failed_wal_sync_fences_the_engine_with_the_reason`; `group_commit.rs`: `a_failed_group_sync_fences_the_engine_with_the_reason`); node level (`node.rs`): `an_engine_that_cannot_write_withdraws_write_readiness_only`, `only_a_fault_fence_makes_the_leader_give_up_its_lease` | covered by injection. The node reports it: `/readyz/write` goes 503, `node.write_fence` names the reason, `aikoql_node_engine_fenced` feeds `AikoqlWriteFenced`, and under HA the leader stops renewing and exits. Not tested: a real partial write (the tail is cut back, and the engine fences if it cannot — the failed `set_len` branch has no injection), and an end-to-end failover after a fault fence |
| 13 | Disk full on the standby | The frame is not applied and the standby's acked seq does not move, so a synchronous leader's write fails after its timeout; the frame applies when re-sent | storage `tests/replication_log.rs`: `a_standby_that_cannot_append_does_not_advance_what_it_acks` | covered at the engine; no two-process test of the sync leader's refusal under it |
| 14 | Certificate expiry or rotation | An untrusted or expired peer is refused; there is no plaintext fallback off loopback | unit `replication.rs`: `a_standby_that_does_not_trust_the_leader_is_refused`; `replicate_listen_rejects_a_non_loopback_bind` (trust and bind policy; no expiry, no rotation while running) | partial |
| 15 | Clock skew between nodes | No effect on election by design: each side measures elapsed time on its own monotonic clock | none targeted | none — the design claim has no test |
| 16 | Archive outage | Uploads fail; the failure count and the lag are visible on `/status` and the scrape; writes continue; the unuploaded log is held, and the archive catches up to the head when it returns | `an_archive_outage_is_visible_and_heals_without_losing_log` (PROD-6); unit `archive.rs`: `the_archive_holds_log_release_until_it_uploads` | covered for a directory archive; an S3 outage (timeouts rather than errors) is not tested |
| 17 | Promotion during an archive upload | Each term archives its own timeline; an unfinished upload is left alone by pruning | unit `archive.rs`: `each_term_archives_its_own_timeline`, `pruning_leaves_an_unfinished_upload_alone`; `real_s3_ha_failover_then_pitr_from_the_bucket` (env-gated) | partial |
| 18 | *(added)* Leader stopped on purpose (SIGTERM, Ctrl-C, the `shutdown` method) | PROD-3: writes are fenced, the leader waits up to 5 s for its standby to hold the final seq, marks the lease released and exits 0; the standby takes over on its next poll (TTL/5) instead of after a full TTL | `ha_a_stopping_leader_releases_its_lease` (stdio); `ha_a_leader_shut_down_over_tcp_releases_its_lease` (PROD-9: every planned exit of `serve` settles); `ha_sigterm_releases_the_lease_and_exits_zero` (unix only); unit `ha.rs`: `a_stopping_holder_marks_its_lease_released`, `a_release_that_loses_the_lease_reports_it`, `a_released_lease_is_due_at_once_and_live_again_once_resumed` | covered — the SIGTERM test runs on Linux CI only; the Windows console-control path is untested. **Known consequence (chosen, not a gap):** if the 5 s ack wait times out the release still carries the final seq, and a standby behind it does not lead (unit `ha.rs`: `a_lagging_standby_does_not_lead_from_a_released_lease`) — the cluster has no writer until the old node is restarted; the runbook says so. The fence is linearized with in-flight commits (`fence_linearized.rs`), so the released seq is never below an acknowledged write |
| 19 | *(added)* Leader process paused longer than the TTL, then resumed (VM stall, SIGSTOP) | The standby has taken over. On resume the old leader's next renewal conflicts and it fences. Between resume and that renewal it may accept writes: sync mode cannot ack them (its standby is gone or on a newer term); async mode could acknowledge writes that are then discarded by re-bootstrap | unit `replication.rs`: `a_standby_refuses_an_older_term_leader_which_then_fences_itself`; `a_diverged_old_leader_rejoins_the_promoted_standby_by_itself` | partial — the window itself is untested and is a hypothesis from reading `ha.rs` `renew` |
| 20 | *(added)* Enterprise identity across promotion | Users, roles and the bootstrap admin are rows of the replicated store (PROD-2); a standby refuses user writes | `ha_an_oauth_sign_in_survives_a_failover`, `ha_users_and_role_changes_survive_a_failover` | covered for users and roles; none for an OIDC sign-in or the tool audit log across promotion |

## 2. What the matrix says

- **Well covered:** process death, lease loss and self-fencing, sync
  acknowledgement, WAL-gap re-bootstrap, a planned stop (18), a full
  restart (11), an archive outage (16), and a refused WAL append on
  either node (12, 13).
- **Found by writing the tests:** row 12 was a real defect. A failed WAL
  append consumed its seq, leaving a hole in the replication log; the
  seq is now taken only once the frame is durable, and a failed append
  cuts the WAL's tail back (PROD-6).
- **Fixed in both write paths:** the group-commit path
  (`DurabilityMode::GroupCommit`) had the same defect as row 12 and got
  the same fix and test in PROD-9
  (`a_failed_group_append_takes_no_seq_and_the_next_group_is_contiguous`):
  a group that is not made durable gives its seqs back and its bytes are
  cut from the WAL; a committer that cannot cut them stops taking
  writes. The server runs the default `Sync` mode, so it never hit the
  group path.
- **No test at all:** interrupted bootstrap (9), clock skew (15).
- **Tested next door, not on the fault:** a held partition (5), a crash
  during a checkpoint (8), certificate expiry and rotation (14).
- **Row 19 still needs a test before anyone reasons further about it.**
  It needs a paused process (SIGSTOP), so it is unix-only; if the async
  window is real it belongs in the published async contract.

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

## 4. Remaining tests, in order

1. Row 19 (process pause) — decides the async contract; unix-only.
2. Row 12, a real partial write — needs a failing filesystem (a full
   tmpfs on Linux CI), not the pre-write injection hook.
3. Row 5 (held partition) and row 13 across two processes — the
   existing fault proxy (`crates/sdk/fault-proxy`) in front of the
   replication address.
4. Rows 9, 14, 8, 15.
