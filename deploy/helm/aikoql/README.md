# aikoql Helm chart (AG-51)

Leader + standby on the Kubernetes lease: two replicas of the same
binary; whoever holds the lease (`k8s://<namespace>/<leaseName>`)
serves writes, the other follows the holder's replication stream
(TLS) and serves reads (with a `NOT_LEADER` hint to SDKs). MCP is
Streamable HTTP on the metrics port (`/mcp`, bearer = the auth token;
`/metrics` for Prometheus; `/livez` and `/readyz` for the probes).

## Install

```sh
kubectl create secret generic aikoql-auth --from-literal=token='<token>' \
  --from-literal=admin-password='<admin password>'   # required: arms OAuth (the server fails closed without it)
helm install aikoql deploy/helm/aikoql
```

- `auth.existingSecret` is required — the token is never a values
  default. The same token authorizes `/mcp` and the replication stream
  (`TOKEN::admin`).
- Replication between pods is TLS. With `tls.auto` (default) the chart
  renders a CA and one shared serving cert whose SANs cover both pods'
  DNS names into `<release>-repl-tls`, and reuses it across upgrades so
  the CA is stable. Set `tls.auto: false` and provide that Secret
  yourself for a real CA (per-pod certs would be a cert-manager step).
- `appVersion` tracks the workspace version — `scripts/bump-version.py`
  bumps this Chart.yaml too (pinned by `tests/helm_chart.rs`).

## Probes and stopping

- Startup and liveness: `/livez` — the process answers HTTP. A slow
  metrics scrape never restarts a pod.
- Readiness: `/readyz` — the node serves reads. Both the leader and the
  standby are ready; a fenced or stopping node is not. The body names the
  role (`leader`, `standby`, `fenced`, `standalone`) and the term.
- `/readyz/write` is 200 on the lease holder only, and not while its
  engine refuses writes (a disk fault fences it; a leader in that state
  also stops renewing the lease and exits for the supervisor). The chart
  does not probe it — the standby must stay Ready for reads.
- **The chart's Service does not route writes to the leader.** Kubernetes
  readiness is per pod and has no second probe, so the in-cluster default
  sends roughly half of all writes to the standby, which refuses them
  (503 naming the leader). For a working write endpoint use one of: a
  load balancer, ingress or gateway whose active health check is
  `GET /readyz/write`; or a client that follows the leader named in the
  refusal or the lease. Labelling the leader's own pod (so a Service can
  select it) needs a small RBAC grant and is not implemented.
- A terminating pod gets `terminationGracePeriodSeconds` (default 30)
  for its planned stop: writes are fenced, the leader waits at most 5 s
  for the standby to hold its final seq, releases the lease and exits 0.
  The standby then takes over on its next lease poll, not after a full
  TTL.

## Known limits

- One shared serving cert for both pods, and no `--advertise` client
  hint in the lease (SDKs get the replication address as the
  `NOT_LEADER` hint).
- `/metrics` is unauthenticated (in-cluster reachable); the MCP bearer
  token is what protects knowledge.
- No HPA or PodDisruptionBudget yet — two replicas is the HA story.

## Backups: PVC snapshots

Each pod's database lives on its own `ReadWriteOnce` PVC
(`data-aikoql-0`, `data-aikoql-1`). The primary backup is a snapshot of
the **leader's** PVC (`kubectl get lease <leaseName> -o
jsonpath='{.spec.holderIdentity}'` names the leader pod):

```sh
cat <<EOF | kubectl apply -f -
apiVersion: snapshot.storage.k8s.io/v1
kind: VolumeSnapshot
metadata:
  name: aikoql-data-$(date +%Y%m%d%H%M)
spec:
  volumeSnapshotClassName: <your-snapshot-class>
  source:
    persistentVolumeClaimName: data-aikoql-0   # the leader pod's PVC
EOF
```

Restore: create a PVC from the snapshot, and either swap the
StatefulSet's PVC (delete the pod, scale down first) or start a
one-off pod (`kubectl run`) with the snapshot PVC mounted and run
`aikoql serve /data/db ...` for an export.

## The failover gate

`scripts/kind-failover-e2e.sh` (nightly, `benchmark.yml`): a kind
cluster, this chart, a `remember` through the leader, the leader pod
killed, and the promoted standby serving the read — plus an OAuth
sign-in issued before the kill that keeps working against the new
leader (grants live in the replicated database).
