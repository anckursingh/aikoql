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
    // probes on the HTTP surface (PROD-4 pins their paths below)
    assert!(sts.contains("readinessProbe"), "a readiness probe");
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
fn ag51_helm_lint_render_is_wired_into_pr_ci() {
    let ci = read(&root().join(".github/workflows/ci.yml"));
    assert!(
        ci.contains("helm lint deploy/helm/aikoql"),
        "ci.yml must helm-lint the chart on every PR (the kind gate only renders nightly)"
    );
    assert!(
        ci.contains("helm template") && ci.contains("deploy/helm/aikoql"),
        "ci.yml must helm-render the chart on every PR"
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
    assert!(
        secret.contains("lookup"),
        "the CA must survive upgrades: {secret}"
    );
    assert!(
        secret.contains("genCA") && secret.contains("genSignedCert"),
        "a fresh install generates a CA + serving cert: {secret}"
    );
    // the statefulset mount contract: /tls/{ca.crt,tls.crt,tls.key}
    assert!(secret.contains("ca.crt"), "ca.crt key: {secret}");
    assert!(
        secret.contains("tls.crt"),
        "tls.crt key (server chain): {secret}"
    );
    assert!(secret.contains("tls.key"), "tls.key key: {secret}");
}

#[test]
fn ag51_arms_the_admin_password_that_the_surface_fails_closed_on() {
    // first real run, second crash: AIKOQL_ALLOW_REMOTE_HTTP=1 without
    // credentials — the server refuses to start. The chart must demand
    // auth.adminPasswordKey at render time, and the gate must set it.
    let sts = read(&root().join("deploy/helm/aikoql/templates/statefulset.yaml"));
    assert!(
        sts.contains("required") && sts.contains(".Values.auth.adminPasswordKey"),
        "adminPasswordKey must be required at render: {sts}"
    );
    let script = read(&root().join("scripts/kind-failover-e2e.sh"));
    assert!(
        script.contains("adminPasswordKey") && script.contains("admin-password"),
        "the gate must arm the admin password: {script}"
    );
}

#[test]
fn ag51_surface_is_tls_and_the_gate_speaks_https() {
    // first real run, third crash: --metrics-addr on 0.0.0.0 serves
    // logins and session tokens — the server refuses plaintext there
    // (fail-closed). The chart must arm the surface TLS from the same
    // mounted secret, probes must probe HTTPS, and the gate must curl it.
    let sts = read(&root().join("deploy/helm/aikoql/templates/statefulset.yaml"));
    assert!(
        sts.contains("scheme: HTTPS"),
        "probes must probe the TLS surface: {sts}"
    );
    let script = read(&root().join("scripts/kind-failover-e2e.sh"));
    assert!(
        script.contains("https://127.0.0.1:4318"),
        "the gate must curl the TLS surface: {script}"
    );
    // the server requires the session header on every post-initialize MCP
    // call (pinned by the claude.ai tunnel run) — the gate's tools/call
    // must send it or remember dies with a 400. The -H form, not the
    // comment in session().
    assert!(
        script.contains("-H \"Mcp-Session-Id: $"),
        "post-initialize MCP calls need the session header: {script}"
    );
    // a StatefulSet resurrects a killed pod, and the recreated pod
    // reclaims its own pre-restart lease (same --ha-node) before the
    // standby's staleness window — the standby promotion this gate proves
    // requires the controller to be orphaned first
    assert!(
        script.contains("cascade=orphan"),
        "the gate must orphan the StatefulSet before the kill: {script}"
    );
    // MCP sessions are process-local — the promoted leader knows nothing
    // of the pre-kill session ids. What survives is the grant (the token)
    // and the writes; the gate must re-initialize both sessions.
    assert!(
        script.contains("SID2=\"$(session \"$TOKEN\")\""),
        "the gate must re-initialize the static-token session after failover: {script}"
    );
}

/// PROD-4 (G5): the probes ask the questions they are named for —
/// liveness on /livez (the process answers HTTP), readiness on /readyz
/// (the node serves reads, whatever its role). Neither scrapes /metrics:
/// a slow scrape must not restart a pod, and a standby is ready.
#[test]
fn prod4_probes_use_the_health_endpoints_not_the_scrape() {
    let sts = read(&root().join("deploy/helm/aikoql/templates/statefulset.yaml"));
    let probe_path = |probe: &str| -> String {
        let at = sts.find(probe).unwrap_or_else(|| panic!("no {probe}"));
        let rest = &sts[at..];
        let line = rest
            .lines()
            .find(|l| l.trim_start().starts_with("path:"))
            .unwrap_or_else(|| panic!("{probe} has no path"));
        line.trim().trim_start_matches("path:").trim().to_string()
    };
    assert_eq!(probe_path("livenessProbe"), "/livez");
    assert_eq!(probe_path("readinessProbe"), "/readyz");
    assert_eq!(probe_path("startupProbe"), "/livez");
    assert!(
        sts.contains("terminationGracePeriodSeconds"),
        "the stop budget (PROD-3's drain + lease release) is explicit"
    );
}

/// PROD-7 (G8): the alert rules are an asset with laws of their own.
/// Every alert names a severity and links a runbook section that exists;
/// every `aikoql_*` series an expression reads is one the server really
/// exports (a renamed series must fail here, not page nobody at 3 a.m.).
#[test]
fn prod7_alert_rules_link_real_runbooks_and_read_real_series() {
    let rules = read(&root().join("deploy/prometheus/aikoql-alerts.yaml"));
    let runbooks = read(&root().join("docs/RUNBOOKS.md"));
    let src: String = ["http.rs", "node.rs", "replication.rs"]
        .iter()
        .map(|f| read(&root().join("crates/services/api/mcp/src").join(f)))
        .collect();

    let alerts: Vec<&str> = rules
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- alert: "))
        .collect();
    assert!(
        alerts.len() >= 12,
        "the rule file lists its alerts: {alerts:?}"
    );
    let count = |needle: &str| {
        rules
            .lines()
            .filter(|l| l.trim().starts_with(needle))
            .count()
    };
    assert_eq!(count("severity: "), alerts.len(), "a severity per alert");
    assert_eq!(count("runbook_url: "), alerts.len(), "a runbook per alert");

    let anchors: Vec<String> = runbooks
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(|h| h.trim().to_lowercase().replace(' ', "-"))
        .collect();
    for line in rules
        .lines()
        .filter(|l| l.trim().starts_with("runbook_url: "))
    {
        let anchor = line.rsplit('#').next().unwrap().trim();
        assert!(
            line.contains("docs/RUNBOOKS.md#") && anchors.iter().any(|a| a == anchor),
            "no runbook section for {line:?} (sections: {anchors:?})"
        );
    }
    // every alert is the heading of its own section
    for alert in &alerts {
        assert!(
            anchors.contains(&alert.to_lowercase()),
            "RUNBOOKS.md has no section for {alert}"
        );
    }

    let mut series: Vec<&str> = rules
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| t.starts_with("aikoql_"))
        .collect();
    series.sort_unstable();
    series.dedup();
    assert!(
        series.len() >= 8,
        "the rules read the server's series: {series:?}"
    );
    for name in series {
        assert!(src.contains(name), "no server code exports {name}");
    }
}

/// PROD-7: a PodDisruptionBudget keeps a voluntary disruption (a node
/// drain, a cluster upgrade) from evicting the leader and its standby
/// together. `maxUnavailable: 1` of two never blocks draining one node.
#[test]
fn prod7_a_pdb_allows_one_member_down_at_a_time() {
    let pdb = read(&root().join("deploy/helm/aikoql/templates/pdb.yaml"));
    assert!(pdb.contains("kind: PodDisruptionBudget"));
    assert!(pdb.contains("apiVersion: policy/v1"));
    assert!(
        pdb.contains("maxUnavailable: 1"),
        "one at a time, never zero"
    );
    assert!(
        pdb.contains(".Values.podDisruptionBudget.enabled"),
        "an operator can turn it off"
    );
    let values = read(&root().join("deploy/helm/aikoql/values.yaml"));
    assert!(values.contains("podDisruptionBudget:"));
}
