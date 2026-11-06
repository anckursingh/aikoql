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

// ---- Task 9: the Studio download route ----

use std::io::{Read, Write};
use std::net::TcpStream;

/// One HTTP request over plain TCP → (status, body).
fn http(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let raw = String::from_utf8_lossy(&out);
    let status: u16 = raw
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// POST /api/login as the bootstrap admin ("pw") → session token.
fn admin_token(port: u16) -> String {
    let (status, body) = http(
        port,
        "POST",
        "/api/login",
        r#"{"username":"admin","password":"pw"}"#,
    );
    assert_eq!(status, 200, "{}", body);
    serde_json::from_str::<serde_json::Value>(&body).expect("json")["token"]
        .as_str()
        .expect("token")
        .to_string()
}

#[test]
fn http_export_serves_the_attachment_and_unlicensed_404s() {
    // Task 9: GET /api/audit-export streams the (retention-truncated) audit
    // log as a Content-Disposition attachment for an admin session under
    // the audit-export flag; unlicensed servers keep the v0.4.0 plain 404.
    // (The license also carries multi-user: the admin-only gate resolves
    // the admin role from the store — see the I-2 store-less refusal.)
    let db = tmp("http-export-db");
    let log = tmp("http-export-log");
    seed(
        &db,
        &[("alpha-seed", now_secs()), ("beta-seed", now_secs())],
    );
    let (lic, pk_b64) = write_license(&["audit-export", "multi-user"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let envs = [
        ("AIKOQL_LICENSE", lic_str.as_str()),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
    ];
    let srv = common::spawn_server(&db, &log, &envs, &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");
    let token = admin_token(srv.port);
    let (status, body) = http(
        srv.port,
        "GET",
        &format!("/api/audit-export?token={token}"),
        "",
    );
    assert_eq!(status, 200, "{}", body);
    assert_eq!(
        body,
        audit_log_text(&db),
        "the export is the log, byte-for-byte"
    );
    // the attachment header the browser download keys on
    let raw = common::http_req(
        srv.port,
        &format!(
            "GET /api/audit-export?token={token} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        ),
    )
    .expect("raw response");
    assert!(
        raw.contains("Content-Disposition: attachment"),
        "raw: {raw}"
    );
    // an anonymous GET is refused
    let (anon, _) = http(srv.port, "GET", "/api/audit-export", "");
    assert_eq!(anon, 401);

    // unlicensed: the route never existed in v0.4.0 — plain 404
    let db2 = tmp("http-export-community-db");
    let log2 = tmp("http-export-community-log");
    let srv2 = common::spawn_server(&db2, &log2, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    let health2 = common::wait_health(srv2.port, Instant::now() + Duration::from_secs(60));
    assert!(health2.contains(" 200 "), "health: {health2}");
    let token2 = admin_token(srv2.port);
    let (c_status, c_body) = http(
        srv2.port,
        "GET",
        &format!("/api/audit-export?token={token2}"),
        "",
    );
    assert_eq!(c_status, 404, "{}", c_body);
    assert_eq!(c_body, "Not Found\n", "v0.4.0 plain-text 404: {}", c_body);
}

#[test]
fn http_export_refused_when_no_store_exists() {
    // Final review I-2: a license carrying audit-export WITHOUT multi-user
    // opens no user store. The admin-only gate must fail closed — no
    // config-file session (the auth resolver path), any role, may download
    // the full audit JSONL.
    let db = tmp("http-export-nostore-db");
    let log = tmp("http-export-nostore-log");
    seed(&db, &[("alpha-seed", now_secs())]);
    let (lic, pk_b64) = write_license(&["audit-export"]);
    let lic_str = lic.to_string_lossy().into_owned();
    let envs = [
        ("AIKOQL_LICENSE", lic_str.as_str()),
        ("AIKOQL_LICENSE_PUBLIC_KEY", pk_b64.as_str()),
        ("AIKOQL_ADMIN_PASSWORD", "pw"),
    ];
    let srv = common::spawn_server(&db, &log, &envs, &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");
    let token = admin_token(srv.port);
    let (status, body) = http(
        srv.port,
        "GET",
        &format!("/api/audit-export?token={token}"),
        "",
    );
    assert_eq!(status, 403, "no store, no export: {body}");
    assert!(body.contains("admin role required"), "{body}");
}

#[test]
fn community_backup_parity_with_config_file_session() {
    // Final review I-2 round 2: the store-less admin-gate refusal belongs
    // to the flag-gated enterprise surfaces (route_audit_export), not to
    // route_v1. An UNLICENSED server with a config-file user (the auth
    // resolver path) keeps v0.4.0 behavior — backup succeeds.
    let db = tmp("backup-parity-db");
    let log = tmp("backup-parity-log");
    let hashed = Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
        .arg("hash-password")
        .arg("cfg-pw")
        .output()
        .expect("run hash-password");
    assert!(
        hashed.status.success(),
        "hash-password: {}",
        String::from_utf8_lossy(&hashed.stderr)
    );
    let phc = String::from_utf8(hashed.stdout).expect("hash-password stdout");
    let cfg = tmp("backup-parity.toml");
    std::fs::write(
        &cfg,
        format!(
            "[auth]\nusers = [\n  {{ username = \"admin\", hash = \"{}\", roles = [\"admin\"] }},\n]\n",
            phc.trim()
        ),
    )
    .unwrap();
    let srv = common::spawn_server(&db, &log, &[], &["--config", cfg.to_str().unwrap()]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");
    let login = http(
        srv.port,
        "POST",
        "/api/login",
        r#"{"username":"admin","password":"cfg-pw"}"#,
    );
    assert_eq!(login.0, 200, "{}", login.1);
    let token = serde_json::from_str::<serde_json::Value>(&login.1).expect("json")["token"]
        .as_str()
        .expect("token")
        .to_string();
    let (status, body) = http(
        srv.port,
        "POST",
        &format!("/api/v1/backup?token={token}"),
        "{}",
    );
    assert_eq!(
        status, 200,
        "community config-file sessions must back up (v0.4.0 parity): {}",
        body
    );
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
