//! Enterprise audit retention + JSONL export (Task 7). All behavior is
//! verified through the spawned aikoql-mcp binary: retention rotation
//! rewrites {db}.audit.log at serve start ONLY under a license carrying
//! audit-export (community mode leaves the log byte-for-byte untouched),
//! and `audit --export-jsonl PATH` streams the log, denied without the flag.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A scratch path unique to this process, pre-cleaned and swept on thread
/// exit (the sweep also takes the {stem}.audit.log sidecar).
fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("aikoql-audit-export-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    common::register_temp_path(&p);
    p
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Seed {db}.audit.log with JSONL entries in the exact shape audit_log
/// writes (ts = unix seconds) so retention's cutoff math sees real data.
fn seed(db: &Path, entries: &[(&str, u64)]) {
    let mut text = String::new();
    for (detail, ts) in entries {
        text.push_str(
            &serde_json::json!({
                "ts": ts,
                "agent": "seed",
                "tool": "memory_store",
                "outcome": "ok",
                "detail": detail,
            })
            .to_string(),
        );
        text.push('\n');
    }
    std::fs::write(format!("{}.audit.log", db.display()), text).unwrap();
}

fn audit_log_text(db: &Path) -> String {
    std::fs::read_to_string(format!("{}.audit.log", db.display())).unwrap()
}

/// A license file signed by a fresh TEST keypair + the matching
/// AIKOQL_LICENSE_PUBLIC_KEY override for the debug binary. The path is
/// per-call unique: tests run in parallel and must never overwrite each
/// other's license file.
fn write_license(features: &[&str]) -> (PathBuf, String) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (pk, blob) = common::sign("test", None, features);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let lic = std::env::temp_dir().join(format!(
        "aikoql-audit-export-{}-lic-{n}.lic",
        std::process::id()
    ));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let pk_b64 = URL_SAFE_NO_PAD.encode(&pk);
    (lic, pk_b64)
}

/// Spawn a licensed serve, wait for /health, and shut it down so the
/// audit log is stable to read.
fn serve_licensed(db: &Path, log: &Path, retention: Option<&str>) {
    let (lic, pk_b64) = write_license(&["audit-export"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let retention_pair =
        retention.map(|v| (String::from("AIKOQL_AUDIT_RETENTION_DAYS"), v.to_string()));
    let mut envs: Vec<(&str, &str)> = vec![
        ("AIKOQL_LICENSE", lic_str.as_str()),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
    ];
    if let Some((k, v)) = &retention_pair {
        envs.push((k.as_str(), v.as_str()));
    }
    let srv = common::spawn_server(db, log, &envs, &[]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let health = common::wait_health(srv.port, deadline);
    assert!(health.contains(" 200 "), "health: {health}");
    srv.shutdown();
}

/// Run the binary with an enterprise-clean env; capture status + output.
fn run_bin(args: &[&str], envs: &[(&str, &str)]) -> (ExitStatus, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .args(args)
        .env_remove("AIKOQL_LICENSE")
        .env_remove("AIKOQL_LICENSE_PUBLIC_KEY")
        .env_remove("AIKOQL_AUDIT_RETENTION_DAYS")
        .envs(envs.iter().copied())
        .output()
        .expect("run aikoql-mcp");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn retention_rotates_old_entries() {
    // AIKOQL_AUDIT_RETENTION_DAYS=1 → cutoff = now − 1 day. A licensed
    // restart must drop the 3-day-old entry and keep the fresh one.
    let db = tmp("retention-db");
    let log = tmp("retention-log");
    let now = now_secs();
    seed(&db, &[("old-seed", now - 3 * 86_400), ("fresh-seed", now)]);
    serve_licensed(&db, &log, Some("1"));
    let text = audit_log_text(&db);
    assert!(!text.contains("old-seed"), "rotated log: {text}");
    assert!(text.contains("fresh-seed"), "rotated log: {text}");
}

#[test]
fn default_retention_is_90_days() {
    // No env → the 90-day default. A 95-day-old entry rotates out; a
    // 2-day-old entry survives.
    let db = tmp("default-retention-db");
    let log = tmp("default-retention-log");
    let now = now_secs();
    seed(
        &db,
        &[
            ("old-seed", now - 95 * 86_400),
            ("fresh-seed", now - 2 * 86_400),
        ],
    );
    serve_licensed(&db, &log, None);
    let text = audit_log_text(&db);
    assert!(!text.contains("old-seed"), "rotated log: {text}");
    assert!(text.contains("fresh-seed"), "rotated log: {text}");
}

#[test]
fn community_mode_never_rotates() {
    // The v0.4.0 guarantee: without a valid license the retention env
    // changes nothing — the old entry survives byte-for-byte.
    let db = tmp("community-db");
    let log = tmp("community-log");
    let now = now_secs();
    seed(&db, &[("old-seed", now - 3 * 86_400)]);
    let srv = common::spawn_server(&db, &log, &[("AIKOQL_AUDIT_RETENTION_DAYS", "1")], &[]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let health = common::wait_health(srv.port, deadline);
    assert!(health.contains(" 200 "), "health: {health}");
    srv.shutdown();
    assert!(audit_log_text(&db).contains("old-seed"));
}

#[test]
fn export_writes_jsonl_lines() {
    // --export-jsonl streams the audit log: one JSON object per line, and
    // byte-equal to the source.
    let db = tmp("export-db");
    seed(
        &db,
        &[("alpha-seed", now_secs()), ("beta-seed", now_secs())],
    );
    let out = tmp("export-out.jsonl");
    let (lic, pk_b64) = write_license(&["audit-export"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let db_s = db.to_string_lossy().into_owned();
    let out_s = out.to_string_lossy().into_owned();
    let (status, _stdout, stderr) = run_bin(
        &["audit", "--export-jsonl", &out_s, &db_s],
        &[
            ("AIKOQL_LICENSE", lic_str.as_str()),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ],
    );
    assert!(status.success(), "stderr: {stderr}");
    let exported = std::fs::read_to_string(&out).expect("export file written");
    assert!(!exported.is_empty());
    for line in exported.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("one JSON object per line");
    }
    assert_eq!(exported, audit_log_text(&db));
}

#[test]
fn export_refuses_to_overwrite_the_live_log() {
    // Naming the live audit log as the export destination must be refused
    // (non-zero exit) — a create(truncate)-then-copy of the source is a
    // silent evidence wipe that would still print a success message.
    let db = tmp("export-self-db");
    seed(
        &db,
        &[("alpha-seed", now_secs()), ("beta-seed", now_secs())],
    );
    let (lic, pk_b64) = write_license(&["audit-export"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let db_s = db.to_string_lossy().into_owned();
    let log_s = format!("{}.audit.log", db.display());
    let (status, _stdout, stderr) = run_bin(
        &["audit", "--export-jsonl", &log_s, &db_s],
        &[
            ("AIKOQL_LICENSE", lic_str.as_str()),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ],
    );
    assert!(!status.success());
    assert!(stderr.contains("export refused"), "stderr: {stderr}");
    let text = audit_log_text(&db);
    assert!(
        text.contains("alpha-seed") && text.contains("beta-seed"),
        "log after refused export: {text}"
    );
}

#[cfg(unix)]
#[test]
fn export_refuses_when_destination_aliases_the_log() {
    // A symlink to the live log — and a hardlink (same inode) — are the
    // same file; both are refused before File::create can truncate it.
    let db = tmp("export-alias-db");
    seed(&db, &[("alpha-seed", now_secs())]);
    let (lic, pk_b64) = write_license(&["audit-export"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let db_s = db.to_string_lossy().into_owned();
    let log_path = std::path::PathBuf::from(format!("{}.audit.log", db.display()));

    let sym = tmp("export-alias-sym.jsonl");
    std::os::unix::fs::symlink(&log_path, &sym).unwrap();
    let sym_s = sym.to_string_lossy().into_owned();
    let (status, _stdout, stderr) = run_bin(
        &["audit", "--export-jsonl", &sym_s, &db_s],
        &[
            ("AIKOQL_LICENSE", lic_str.as_str()),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ],
    );
    assert!(!status.success());
    assert!(stderr.contains("export refused"), "stderr: {stderr}");
    assert!(audit_log_text(&db).contains("alpha-seed"));

    let hard = tmp("export-alias-hard.jsonl");
    std::fs::hard_link(&log_path, &hard).unwrap();
    let hard_s = hard.to_string_lossy().into_owned();
    let (status, _stdout, stderr) = run_bin(
        &["audit", "--export-jsonl", &hard_s, &db_s],
        &[
            ("AIKOQL_LICENSE", lic_str.as_str()),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ],
    );
    assert!(!status.success());
    assert!(stderr.contains("export refused"), "stderr: {stderr}");
    assert!(audit_log_text(&db).contains("alpha-seed"));
}

#[test]
fn export_requires_enterprise_license() {
    // Unlicensed → the exact enterprise-feature error, non-zero exit, no
    // file. A license WITHOUT audit-export is denied the same way (the
    // flag, not just any license, gates the export).
    let db = tmp("export-denied-db");
    seed(&db, &[("alpha-seed", now_secs())]);
    let out = tmp("export-denied-out.jsonl");
    let db_s = db.to_string_lossy().into_owned();
    let out_s = out.to_string_lossy().into_owned();

    let (status, _stdout, stderr) = run_bin(&["audit", "--export-jsonl", &out_s, &db_s], &[]);
    assert!(!status.success());
    assert!(
        stderr.contains("enterprise feature: audit export"),
        "stderr: {stderr}"
    );
    assert!(!out.exists(), "a denied export must not create the file");

    let (lic, pk_b64) = write_license(&["multi-user"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let (status, _stdout, stderr) = run_bin(
        &["audit", "--export-jsonl", &out_s, &db_s],
        &[
            ("AIKOQL_LICENSE", lic_str.as_str()),
            ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ],
    );
    assert!(!status.success());
    assert!(
        stderr.contains("enterprise feature: audit export"),
        "stderr: {stderr}"
    );
    assert!(!out.exists(), "a denied export must not create the file");
}
