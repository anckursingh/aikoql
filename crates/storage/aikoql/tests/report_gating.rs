//! P3-M0 clb001 — committed `artifacts/` evidence is only rewritten when
//! the report env is armed: a plain local suite run must never dirty
//! committed artifacts (TESTING-PLAN-PHASE3 rule 6). One test fn with both
//! arms sequential — env mutation must not race across parallel tests.

mod common;

#[test]
fn report_write_gates_on_the_env() {
    let dir = common::tmp("report-gating");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("report.md");

    // Unarmed: the write is a no-op.
    std::fs::write(&path, "sentinel").unwrap();
    std::env::remove_var("AIKOQL_REPORT_WRITE");
    common::report_write(&path, "clobber");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "sentinel");

    // Armed: the write lands.
    std::env::set_var("AIKOQL_REPORT_WRITE", "1");
    common::report_write(&path, "clobber");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "clobber");
}
