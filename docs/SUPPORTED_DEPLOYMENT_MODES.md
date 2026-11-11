# Supported deployment modes

**As of:** `main` at `4704d9b6` (2026-11-11). Derived from
`PRODUCTION_READINESS_AUDIT.md`; that document carries the evidence.

"Supported" here means the combination has a defined contract and at
least one automated test. It does not mean production-certified: the
acceptance gates in the productionisation review have not been run.

## 1. Topologies

| Mode | Flags | Status | Durability contract |
|---|---|---|---|
| Single node | none | supported | Local WAL and storage engine durability |
| Single node + archive | `--archive DIR\|s3://…` | supported | As above, plus recovery from the archive. Loss window on total node loss: up to `--archive-log-ms` (default 5 s) of acknowledged writes |
| Leader + standby, asynchronous | `--replicate-listen` on the leader; `--follow` on the standby | supported | Acknowledged writes not yet shipped are lost if the leader is lost |
| Leader + standby, synchronous | add `--replicate-sync-ms N` | supported | A write is acknowledged only after one standby acked it; with no standby it fails after `N` ms. One-standby acknowledgement, not quorum |
| Automatic failover on a lease | `--ha-lease` (file, `s3://`, `k8s://`), `--ha-node`, `--ha-ttl-ms` | supported | One writer per term; the standby takes over after a full TTL (default 10 s) and only if it holds the holder's last renewed seq |
| Read replica / cascading replica | `--follow`, optionally `--replicate-listen` on the replica | supported | Converging reads; writes refused with a `NOT_LEADER` hint |
| Kubernetes, two replicas | `deploy/helm/aikoql` | supported, with the limits in section 4 | As automatic failover on `k8s://` |
| More than two voting members, quorum replication | — | not implemented | — |
| Horizontal autoscaling of the StatefulSet | — | not supported, by design | The members are a leader and its standby, not stateless workers |

Replication beyond loopback requires TLS (`--replicate-tls-cert`,
`--replicate-tls-key`, `--replicate-tls-ca`); a plaintext listener on a
non-loopback address refuses to start.

## 2. Identity modes

| Mode | Single node | Replicated / HA |
|---|---|---|
| Static bearer token (`--tcp-token`) | supported | supported — same Secret on both nodes |
| Config-file logins + OAuth (community) | supported | supported — grants live in the replicated store; a sign-in made before failover keeps working (`ha_an_oauth_sign_in_survives_a_failover`) |
| Enterprise multi-user (`multi-user` licence) | supported | **unsupported — refused at startup (exit 2)** |
| Enterprise OIDC SSO (`oidc-sso` licence, implies multi-user) | supported | **unsupported — refused at startup (exit 2)** |

Why the enterprise rows are unsupported with replication: users and
roles live in `{db}/users.db`, a SQLite file each node owns separately.
If the combination were allowed, then after a promotion:

- users that exist only in that table are refused until they sign in
  again (fail-closed);
- role changes made through the REST API on the old leader are gone;
- a sign-in in progress fails, because OIDC flow state is in memory;
- the tool audit log (`{db}.audit.log`) starts from the new leader's
  own file.

Until the user table moves into the replicated store, a server with a
multi-user or OIDC licence refuses to start with `--replicate-listen`,
`--follow` or `--ha-lease` (PROD-1, test
`multi_user_with_replication_refuses_start`).

## 3. Sessions and clients

- MCP sessions are process-local. Promotion re-executes the server
  process, so every client must reconnect and re-initialise.
- All five SDKs follow a `NOT_LEADER` hint. The Python SDK retries the
  refused call once after re-dialling; `session_init` identity is not
  replayed. Retry and backoff policy for the other SDKs, and behaviour
  when a connection breaks after commit but before the response, are
  not yet documented.

## 4. Kubernetes chart limits

- Two replicas; no PodDisruptionBudget, NetworkPolicy or HPA.
- One serving certificate shared by both pods.
- `/metrics` is unauthenticated and doubles as the liveness and
  readiness probe; neither probe reflects the node's role.
- No `terminationGracePeriodSeconds` or `preStop`, and the server does
  not handle SIGTERM: deleting the leader pod is a hard stop, and
  writes resume only after the lease TTL lapses and the standby
  promotes.
- No licence wiring in the chart.
- PVC snapshot backup is a manual procedure (chart README).

## 5. Encryption and KMS

Encryption at rest with a local key file or an AWS KMS pointer file is
supported on a single node. In a replicated deployment each node opens
its own store and needs its own access to the key or to KMS; a node
that cannot open the key fails at startup. Key rotation and a KMS
outage while running have not been examined for the replicated case.

## 6. Multi-tenancy

Per-tenant quotas and usage accounting exist. A shared multi-tenant
service tier is **not a supported mode**: isolation across every data
path has not been audited. Dedicated instance per customer is the only
isolation model this document supports.

## 7. Managed service

No control plane (provisioning, billing, organisation lifecycle) exists
in this repository. Nothing here describes a hosted offering.
