# Production readiness audit (Phase 0)

**Audited revision:** `main` at `4704d9b6b34ed57bbdb34e19fd5cb8a828380834`
(2026-11-11) — the same commit the productionisation review inspected.
**Scope:** this repository only. **Method:** source and test reading.
No test, chart or cluster was executed for this audit; every row says
how far it was verified.

Companion documents: `SUPPORTED_DEPLOYMENT_MODES.md` (what may be run
together) and `HA_FAILURE_MATRIX.md` (fault scenarios against tests).

Verification levels used below:

- **read** — the code path was read end to end.
- **located** — the symbol or test exists by name; its body was not read.
- **not traced** — not examined yet; listed so it is not mistaken for
  "fine".

## 1. Summary

The review is right that HA, archive, metrics, the Helm chart and the
enterprise identity pieces all exist. Its highest-priority item is
narrower than it reads: OAuth grants already replicate, and only the
enterprise user table and the tool audit log are node-local. The gaps
that matter most to an operator are elsewhere — there is no SIGTERM
handling, the probes do not know the node's role, and the archive
reports nothing.

| # | Gap | Severity | Section |
|---|---|---|---|
| G1 | A multi-user licence combined with replication or `--ha-lease` starts without refusal; the docs call the combination unsupported — **closed**: PROD-1 refused the combination, PROD-2 made it supported | high | 3 |
| G2 | `users.db` is node-local SQLite, outside replication and outside the storage fence — **closed by PROD-2** (users are rows of the replicated system store) | high | 3 |
| G3 | The tool audit log is a node-local file; it does not follow a promotion | high | 3 |
| G4 | No SIGTERM/SIGINT handler: a pod termination is a hard kill and the lease is left to expire — **closed by PROD-3** (`shutdown.rs`: fence, release the lease, exit 0) | high | 6 |
| G5 | Liveness and readiness both probe `/metrics`; `/health` is a static `ok` with no role, lease or dependency state — **closed by PROD-4** for role and traffic class (`/livez`, `/readyz`, `/readyz/write`); dependency health (archive, lease API, KMS) is still not reported | medium | 5 |
| G6 | No archive metrics and no lease/role/term metrics; archive failures are log lines only — **closed by PROD-5** (`aikoql_node_role`, `aikoql_replication_term`, `aikoql_ha_lease_*`, `aikoql_archive_*`). Correction to this audit: archive lag in frames was already exported, as the `peer="archive"` row of `aikoql_replication_standby_lag_frames` | medium | 6 |
| G7 | `status` reports health, metrics and ABI only — nothing about role, lease, lag or archive — **closed by PROD-5** for a running node (`GET /status`); the offline `status` CLI verb is unchanged, and disk free space, KMS state and lease holder/expiry are not in the report yet | medium | 6 |
| G8 | No alert rules, runbooks, restore drill or support bundle — **closed by PROD-7** for alert rules (`deploy/prometheus/aikoql-alerts.yaml`), runbooks (`docs/RUNBOOKS.md`) and the restore drill (`restore-drill`); a redacted support bundle is still not built | medium | 6 |
| G9 | No user-disable operation, so "a disabled user stays disabled after failover" cannot be tested yet | medium | 3 |
| G10 | The Helm chart has no licence wiring, no PodDisruptionBudget, no NetworkPolicy, no termination settings — **partly closed**: termination settings (PROD-4) and a PodDisruptionBudget (PROD-7) are in the chart; licence and archive wiring and a NetworkPolicy are not | low | 5 |
| G11 | Failure-matrix cells with no test: disk full, certificate expiry/rotation, full-cluster restart, replication partition, process pause — **partly closed by PROD-6**: disk full (both nodes, by injection), full restart and archive outage are tested, and the disk-full test found and fixed a seq-hole defect in the WAL write path; partition, certificate lifetime and process pause remain | medium | `HA_FAILURE_MATRIX.md` |

## 2. High availability and replication

| Capability | Evidence | Level |
|---|---|---|
| Lease election with a compare-and-swap store (file, S3, Kubernetes Lease) | `crates/services/api/mcp/src/ha.rs` — `LeaseStore`, `FileLease`, `S3Lease`, `K8sLease` | read |
| Timing never compares clocks across nodes: a standby takes over after a full TTL unchanged on its own monotonic clock; a leader fences at 2/3 TTL since its last renewal started | `ha.rs` module header, `stand_by`, `renew` | read |
| A standby behind the holder's last renewed seq does not take over | `ha.rs` `stand_by` (`last >= rec.seq`) | read |
| Self-fence: stop writes, persist `Fenced`, exit 75 for the supervisor | `ha.rs` `fence`; `FENCED_EXIT` | read |
| The fence is enforced in the storage engine, so every kernel write path is covered | `crates/storage/aikoql-v2/src/db.rs` (`fenced` checked on the write path), `engine.rs` `StorageAdminApi::fence` | read |
| Promotion re-executes the process (unix) | `ha.rs` `win` → `reexec` | read |
| Synchronous acknowledgement: a write is acked only once a standby acked its seq, else it fails after the timeout | `replication.rs` `sync_gate`; `--replicate-sync-ms` in `config.rs` | read |
| Term history, divergence detection, automatic re-bootstrap | `replication.rs` `diverged`, `rebootstrap`, `finish_rebootstrap` | located |
| Replication TLS; plaintext listener is loopback-only | `replication.rs`; tests `replicate_listen_rejects_a_non_loopback_bind`, `replicate_listen_over_tls_accepts_a_non_loopback_bind` | located |
| Read replicas and cascading replicas | tests `a_read_replica_serves_converging_reads_and_refuses_writes`, `a_replica_can_feed_another_replica` | located |
| Replication lag metrics | `aikoql_replication_head_seq`, `aikoql_replication_standbys`, `aikoql_replication_standby_lag_frames` (`replication.rs` `metrics`) | read |

Consequences worth stating plainly:

- **Sync is one-standby acknowledgement, not quorum.** `sync_gate`
  waits for the highest acked seq from any standby.
- **Promotion drops process memory.** Because promotion re-executes,
  MCP sessions and in-flight OIDC sign-in flows
  (`session.rs` `OidcFlows`, in memory) do not survive it.
- **The fence does not cover SQLite or the audit file.** Both are
  written outside the engine (section 3).

Defaults (`config.rs`): `--ha-ttl-ms` 10 000; `--archive-log-ms` 5 000;
`--archive-snapshot-secs` 3 600; sync replication off unless
`--replicate-sync-ms` is set.

## 3. Identity, licensing and audit across HA

> **Status after PROD-2:** rows 1 and 3–7 below describe the audited
> revision. Users and roles now live in the replicated system store
> (`sys/users/`); see `SUPPORTED_DEPLOYMENT_MODES.md` section 2. The
> audit-log rows (8) and gaps G3 and G9 are unchanged.

The review's ten investigation questions, answered from code:

| # | Question | Finding | Level |
|---|---|---|---|
| 1 | Where are user records stored? | `{db}/users.db`, SQLite via rusqlite, opened only under a multi-user licence (`main.rs` after `open_kernel`; `users.rs`) | read |
| 2 | Where are grants, refresh state and session state? | Access and refresh grants: the kernel store, keyed by the token's SHA-256 (`oauth.rs` `Grant`, `token_key`, `store: Arc<dyn StorageEngine>`). Authorization codes, client-metadata cache, OIDC flows and Studio sessions: process memory | read |
| 3 | Which records replicate? | Grants replicate. `users.db`, the audit log, and all in-memory state do not | read |
| 4 | How do user creation and role changes propagate? | They do not. `POST /api/users`, `PUT /api/users/{sub}/roles` and the OIDC callback write the local `users.db` only (`api_rest.rs`, `http.rs` `upsert_oidc_user`) | read |
| 5 | Leader fails right after a user or role mutation? | The mutation is lost; the promoted node has its own `users.db` | read |
| 6 | Can the promoted standby validate pre-failover tokens? | Config-file logins: yes (grant replicated, roles from `AuthResolver`). Users that exist only in `users.db`: no — `OAuth::validate` finds no roles and refuses. Fail-closed | read |
| 7 | Do revocation and role changes hold after failover? | Roles are read per request, so a change is immediate on the node that made it; it is not carried to the other node | read |
| 8 | Are audit records replicated? | Key-lifecycle audit lives in the store under `__audit__/keys/` and replicates (`crates/kernel/src/security/audit.rs`). The tool audit log is `{db}.audit.log`, a local file (`audit.rs` `audit_log_path`) | read |
| 9 | Can bootstrap create conflicting admins? | Each node seeds its own break-glass admin row from `AIKOQL_ADMIN_PASSWORD`; the sub `admin` is reserved against OIDC and REST upserts. Same secret gives the same password with a different hash per node | read |
| 10 | Is KMS available to both nodes after promotion? | A KMS pointer key file opens through AWS KMS at kernel open (`engine.rs`); failure there fails startup. Whether both pods have the pointer file and IAM access is deployment-specific | read (startup path); not traced (rotation, outage mid-run) |

Two facts narrow the problem:

- **OIDC roles are refreshed from the IdP on every sign-in.**
  `upsert_oidc_user` overwrites the stored roles with the mapped claims
  (`http.rs` OIDC callback). For OIDC users the table behaves like a
  cache between sign-ins; a manual `PUT .../roles` lasts until the next
  sign-in.
- **A sign-in survives failover today for config-file logins.** Test
  `ha_an_oauth_sign_in_survives_a_failover`
  (`tests/replication_failover.rs`) and the kind e2e both assert it.

Open points:

- The user routes have no standby or fence check, so a standby appears
  to accept user writes into its own `users.db`. Read from
  `api_rest.rs`; not executed.
- There is no disable or delete operation in `users.rs` (G9).
- Startup has no check that refuses multi-user with
  `--replicate-listen`, `--follow`, or `--ha-lease` (G1).
  `docs/ENTERPRISE-TIERS.md` documents the restriction; nothing
  enforces it.

**Smallest consistent design:** store users in the kernel store beside
the grants. That gives replication, the storage fence, sync
acknowledgement and archive coverage with no new mechanism, and the
grants already have a failover test to copy. Until it lands, refuse the
combination at startup.

## 4. Backup, archive and restore

| Capability | Evidence | Level |
|---|---|---|
| Object-store seam: directory and S3-compatible | `archive.rs` `ObjectStore`, `open_store`; `s3.rs` | located |
| Log batches per term, periodic snapshots, pruning that keeps what the newest snapshots need | `archive.rs` `pass`, `snapshot`, `prune`; tests `each_term_archives_its_own_timeline`, `pruning_keeps_the_newest_snapshots_and_what_they_need`, `pruning_leaves_an_unfinished_upload_alone` | located |
| Restore to latest, to a seq, to a time | `archive.rs` `restore`, `replay`; tests `archive_then_restore_to_the_latest_and_to_a_seq`, `restore_until_a_time_stops_at_what_was_archived_by_then`, `pitr_restores_the_archive_to_the_latest_and_to_a_point_in_time` | located |
| Log release is held until the archive uploaded it | test `the_archive_holds_log_release_until_it_uploads` | located |
| Real S3 and real Kubernetes legs, env-gated | tests `real_s3_*`, `real_k8s_*`; `scripts/s3-it.sh`, `scripts/k8s-it.sh` | located |

Gaps: the archive loop only logs failures (`warn!` in `archive.rs`
`run`); it exports no metric for last upload, lag or snapshot age (G6).
Because log release waits for upload, an archive outage grows the
leader's retained log; nothing surfaces that except disk usage. There is
no automated restore drill (G8). `users.db` and the audit log are not in
the archive.

## 5. Kubernetes and Helm

| Item | State | Evidence |
|---|---|---|
| Two-replica StatefulSet on a Kubernetes Lease | present | `deploy/helm/aikoql/templates/statefulset.yaml` |
| Lease RBAC limited to `get`, `create`, `update` on leases | present, pinned | `templates/rbac.yaml`; test `ag51_rbac_grants_only_the_lease_verbs` |
| Replication TLS with a helm-rendered CA, stable across upgrades | present, pinned | `templates/repl-tls.yaml`; test `ag51_repl_tls_is_a_helm_managed_secret_with_a_stable_ca` |
| Token and admin password from a Secret, never values | present, pinned | `values.yaml` `auth.existingSecret`; test `ag51_arms_the_admin_password_that_the_surface_fails_closed_on` |
| `helm lint` and `helm template` in PR CI | present | `.github/workflows/ci.yml` |
| kind failover e2e, nightly | present | `scripts/kind-failover-e2e.sh`; `.github/workflows/benchmark.yml` job `kind-failover` |
| Probes | both `httpGet /metrics`; no startup probe | `statefulset.yaml` (G5) |
| Role-aware readiness | absent | `/health` in `http.rs` returns a static body (G5) |
| `terminationGracePeriodSeconds`, `preStop` | absent — and the binary has no signal handler to honour them | `statefulset.yaml` (G4) |
| PodDisruptionBudget, NetworkPolicy, HPA | absent; HPA should stay absent for this topology | chart README "Known limits" (G10) |
| Per-pod certificates | absent; one shared serving cert | chart README |
| `/metrics` authentication | none; relies on in-cluster reachability | chart README |
| Licence wiring (`AIKOQL_LICENSE`) | absent from the chart, so an enterprise install needs a chart change — which is also where G1 would bite | `statefulset.yaml` (G10) |
| Chart tests | static laws over the YAML text only; no rendered-manifest assertions | `tests/helm_chart.rs` |

The StatefulSet sets no `updateStrategy`, `podManagementPolicy` or
`startupProbe`, so it runs on the Kubernetes defaults. Not traced:
rolling-upgrade behaviour with a live leader, and PVC rescheduling.

## 6. Observability and operations

| Item | State | Evidence |
|---|---|---|
| Prometheus metrics: tools, storage, compaction backlog, WAL bytes, replication lag | present | `http.rs` `prometheus_metrics`; `replication.rs` `metrics`; `tests/observability.rs` |
| OTLP tracing, structured logs | present | `observability.rs` (located) |
| Lease, role, term, fence metrics | absent | no `aikoql_ha_*` or equivalent (G6) |
| Archive metrics | absent | `archive.rs` exports none (G6) |
| `status` verb | health, metrics, ABI version only | `cli.rs` `Some("status")` (G7) |
| Graceful shutdown | only through the protocol `shutdown` method and stdio EOF; no OS signal handling anywhere in `crates/services/api/mcp/src` | `transport.rs` `SHUTDOWN_FLAG` (G4) |
| Lease release on shutdown | absent — a stopping leader's lease expires by TTL | `ha.rs` has no release path (G4) |
| Alert rules, runbooks, restore drill, support bundle | none found under `docs/`, `deploy/`, `scripts/`, `.github/` (search of those trees was partial; hits were prose mentions only) | (G8) |

## 7. SDK failover

All five SDKs carry `NOT_LEADER` handling (`crates/sdk/{python,go,
rust,typescript,java}`). Dedicated tests were found for Python
(`tests/test_leader_hint.py`), Go (`client_test.go`), TypeScript
(`tests/leader-hint.test.ts`) and Java (`LeaderHintTest.java`); none
under `crates/sdk/rust/tests/`.

Python was read (`mcp_client.py` `call_tool`, `_follow`): on a
`NOT_LEADER` refusal with a hint it re-dials the hinted address with the
same TLS context, re-runs `initialize`, and retries the call once.
That retry is safe because the refusal means the write was not applied.
There is no backoff or jitter, and `session_init` identity is not
replayed. The other four SDKs are located only.

Not traced: behaviour when the connection breaks after commit but before
the response (the ambiguous-outcome case), hint address validation in
each SDK, and whether any SDK retries on plain connection loss.

## 8. Tenant isolation and metering

`crates/kernel/src/security/tenant.rs` keeps per-tenant object counts
as store rows with a head watermark and enforces quotas. That is
accounting. Isolation across scans, graph traversal, vector retrieval,
caches, background jobs, archive paths, metrics and logs is **not
traced** in this audit, and nothing here should be read as evidence
for a shared multi-tenant tier.

## 9. Managed-service control plane

No provisioning, billing, subscription or organisation-lifecycle code
was found in this repository. Per the review, that is "not found here",
not "missing": other repositories were not available to this audit.

## 10. Test and CI inventory

| Suite | Where | Runs |
|---|---|---|
| Failover, fencing, sync ack, PITR, replicas (22 tests) | `crates/services/api/mcp/tests/replication_failover.rs` | workspace tests in `ci.yml`; `real_s3_*` and `real_k8s_*` are env-gated |
| Lease store contract, resume, standby (unit) | `ha.rs` `mod tests` | workspace tests |
| Stream, term, divergence, TLS, lag (unit) | `replication.rs` `mod tests` | workspace tests |
| Archive, prune, restore (unit) | `archive.rs` `mod tests` | workspace tests |
| Users, OIDC, OAuth, RBAC, licence | `tests/users.rs`, `oidc_sso.rs`, `mcp_oauth.rs`, `rbac_rest.rs`, `license.rs` | workspace tests |
| Chart static laws | `tests/helm_chart.rs` | workspace tests |
| `helm lint` / `helm template` | `ci.yml` | every PR |
| kind failover e2e | `scripts/kind-failover-e2e.sh` | nightly, `benchmark.yml` |

## 11. What this audit did not do

- Run any test, chart render or cluster.
- Trace tenant isolation, KMS rotation and mid-run outage, the archive
  internals, four of the five SDKs, or rolling upgrades.
- Look outside this repository.

These are inputs to scoping, in order: the users-in-store change (needs
the standby user-route behaviour confirmed by a test), the failure
matrix work (needs the located tests read), and any tenant-tier claim.
