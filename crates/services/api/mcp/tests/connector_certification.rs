//! MVP-QA-001 Suite D/E — connector certification against LIVE databases
//! (GATE-04). One test file, one TDD item per test group; red first, then
//! the production fix, never the expected results.
//!
//! Runs for real in the CI `connectors` job (services: pg/pgvector/mongo/
//! neo4j); env-skip locally unless `docker compose --profile full up -d`
//! plus the AIKOQL_TEST_* vars (see connectors/mod.rs).

mod connectors;

/// Item 1 acceptance: every configured live source is reachable through the
/// harness probes. Red here means compose/CI wiring is broken, not the
/// product — it guards the other tests from silently skipping in CI.
#[test]
fn infra_live_db_connectivity() {
    let _ = connectors::Live::pg();
    let _ = connectors::Live::pgvector();
    let _ = connectors::Live::mongo();
    let _ = connectors::Live::neo4j();
}
