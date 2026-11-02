//! AG-51 Helm chart laws (artifact style, like w31_oss_002): the chart
//! must exist at the mandated path and wire the HA topology the gate
//! depends on — leader+standby on the k8s lease, per-pod PVC, pod DNS for
//! `--replicate-advertise`, lease RBAC, probes on /metrics — and the
//! kind-cluster failover e2e must exist and be wired into the nightly
//! owner (benchmark.yml). The real render + failover gate runs in CI
//! (kind cluster, helm template/install); these are the static laws.
//! String assertions only, w31_oss style — no YAML parser.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn read(p: &PathBuf) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{} missing: {e}", p.display()))
}

#[test]
fn ag51_chart_exists_at_the_mandated_path_with_metadata() {
    let chart = root().join("deploy/helm/aikoql");
    assert!(
        chart.is_dir(),
        "deploy/helm/aikoql/ missing — the AG-51 chart"
    );
    let chart_yaml = read(&chart.join("Chart.yaml"));
    assert!(chart_yaml.contains("apiVersion: v2"), "helm apiVersion v2");
    assert!(chart_yaml.contains("name: aikoql"), "chart name");
    // the shipped appVersion tracks the workspace version (the bump script
    // must bump the chart too — see the chart README)
    let app = format!("appVersion: \"{}\"", env!("CARGO_PKG_VERSION"));
    assert!(
        chart_yaml.contains(&app),
        "appVersion must match {app}: {chart_yaml}"
    );
    assert!(chart.join("values.yaml").is_file(), "values.yaml missing");
    assert!(chart.join("templates").is_dir(), "templates/ missing");
}

#[test]
fn ag51_statefulset_wires_leader_standby_on_the_k8s_lease() {
    let sts = read(&root().join("deploy/helm/aikoql/templates/statefulset.yaml"));
    // leader + standby: two replicas, the k8s lease decides who serves writes
    assert!(sts.contains("replicas: 2"), "leader+standby = 2 replicas");
    assert!(
        sts.contains("--ha-lease") && sts.contains("k8s://"),
        "--ha-lease with a k8s:// URL"
    );
    // a unique node name per pod, from the downward API
    assert!(
        sts.contains("--ha-node") && sts.contains("fieldRef") && sts.contains("metadata.name"),
        "--ha-node from the pod's name"
    );
    // standbys dial the holder's advertised address — pod DNS via the
    // headless service
    assert!(
        sts.contains("--replicate-advertise"),
        "--replicate-advertise on pod DNS"
    );
    // replication follows with the admin token
    assert!(sts.contains("--tcp-token"), "replication token");
    // probes on the metrics surface, not the MCP surface
    assert!(
        sts.contains("readinessProbe") && sts.contains("/metrics"),
        "readiness probe on /metrics"
    );
    // one PVC per pod for the database
    assert!(
        sts.contains("volumeClaimTemplates"),
        "per-pod PVC (volumeClaimTemplates)"
    );
}

#[test]
fn ag51_rbac_grants_only_the_lease_verbs() {
    let rbac = read(&root().join("deploy/helm/aikoql/templates/rbac.yaml"));
    assert!(
        rbac.contains("verbs: [\"get\", \"create\", \"update\"]"),
        "leases get/create/update: {rbac}"
    );
    assert!(rbac.contains("leases"), "leases resource");
    // R5a: nothing but the lease
    assert!(!rbac.contains("pods"), "no pod access needed");
    // the first real kind run caught a secrets rule sneaking past this law:
    // the TLS hook wrote the repl-tls Secret — helm renders it now
    assert!(!rbac.contains("secrets"), "lease-only: {rbac}");
}

#[test]
fn ag51_headless_service_gives_pod_dns() {
    let svc = read(&root().join("deploy/helm/aikoql/templates/service.yaml"));
    assert!(svc.contains("clusterIP: None"), "headless for pod DNS");
}

#[test]
fn ag51_kind_failover_e2e_is_wired_into_the_nightly_owner() {
    let script = root().join("scripts/kind-failover-e2e.sh");
    assert!(
        script.is_file(),
        "scripts/kind-failover-e2e.sh missing — the kind failover gate"
    );
    let owner = read(&root().join(".github/workflows/benchmark.yml"));
    assert!(
        owner.contains("kind-failover-e2e.sh"),
        "benchmark.yml must run the kind failover e2e"
    );
}

#[test]
fn ag51_repl_tls_is_a_helm_managed_secret_with_a_stable_ca() {
    // the first real kind run died in the pre-install Job: alpine/openssl
    // has no kubectl, and kubectl apply needed secret verbs the RBAC never
    // granted. Helm renders the CA instead — no Job, no kubectl-in-a-pod,
    // lease-only RBAC — and lookup keeps the CA stable across upgrades.
    let hook = root().join("deploy/helm/aikoql/templates/tls-hook.yaml");
    assert!(!hook.exists(), "no hook Job: {}", hook.display());
    let secret = read(&root().join("deploy/helm/aikoql/templates/repl-tls.yaml"));
    assert!(secret.contains("lookup"), "the CA must survive upgrades: {secret}");
    assert!(
        secret.contains("genCA") && secret.contains("genSignedCert"),
        "a fresh install generates a CA + serving cert: {secret}"
    );
    // the statefulset mount contract: /tls/{ca.crt,tls.crt,tls.key}
    assert!(secret.contains("ca.crt"), "ca.crt key: {secret}");
    assert!(secret.contains("tls.crt"), "tls.crt key (server chain): {secret}");
    assert!(secret.contains("tls.key"), "tls.key key: {secret}");
}
