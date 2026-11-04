//! Enterprise license verification (Task 2). aikoql-mcp is a binary crate
//! (no lib target to link), so the module under test is compiled directly
//! into this test crate. Fixtures are signed with a TEST keypair generated
//! here (ring) — never the production key, whose private half is off-repo —
//! and verified through `verify_license_with_key` against that pair's public
//! half. The production entry point `verify_license` pins
//! `LICENSE_PUBLIC_KEY_B64`.

#[path = "../src/license.rs"]
#[allow(dead_code)] // production-only items (verify_license, LICENSE_STATE, …)
mod license;

mod common;
use common::sign;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::time::{SystemTime, UNIX_EPOCH};

/// Swap one base64url char mid-segment for a DIFFERENT alphabet char: the
/// decode always succeeds and the decoded bytes always change, so the
/// failure comes from ed25519 verification — never from base64 canonicality
/// (flipping the envelope's last byte can leave the signature untouched,
/// since the final char carries two ignored padding bits).
fn tamper_mid_segment(blob: &mut [u8], segment: usize) {
    let dot = blob
        .iter()
        .position(|b| *b == b'.')
        .expect("envelope separator");
    let (start, len) = if segment == 0 {
        (0, dot) // payload
    } else {
        (dot + 1, blob.len() - dot - 1) // signature
    };
    let mid = start + len / 2;
    blob[mid] = if blob[mid] == b'A' { b'B' } else { b'A' };
}

#[test]
fn valid_key_parses() {
    let (pk, blob) = sign("test", None, &["all"]);
    let state = license::verify_license_with_key(&blob, &pk).expect("valid license");
    assert_eq!(state.org, "test");
    assert!(state.expires.is_none());
    assert!(state.features.oidc_sso && state.features.multi_user && state.features.audit_export);
}

#[test]
fn tampered_key_rejected() {
    let (pk, mut blob) = sign("test", None, &["all"]);
    tamper_mid_segment(&mut blob, 1); // mid-signature, never the last byte
    let err = license::verify_license_with_key(&blob, &pk).unwrap_err();
    assert!(
        matches!(err, license::LicenseError::Signature),
        "got {err:?}"
    );
}

#[test]
fn tampered_payload_rejected() {
    let (pk, mut blob) = sign("test", None, &["all"]);
    tamper_mid_segment(&mut blob, 0); // mid-payload: the signature no longer covers it
    let err = license::verify_license_with_key(&blob, &pk).unwrap_err();
    assert!(
        matches!(err, license::LicenseError::Signature),
        "got {err:?}"
    );
}

#[test]
fn malformed_base64_rejected() {
    let (pk, _) = sign("test", None, &["all"]);
    assert!(license::verify_license_with_key(b"not-a-license", &pk).is_err());
}

#[test]
fn trailing_newline_is_tolerated() {
    // the keygen prints the envelope with println! — a license file on
    // disk ends in '\n' and must still verify (first-boot field report)
    let (pk, blob) = sign("test", None, &["all"]);
    let mut on_disk = blob;
    on_disk.push(b'\n');
    let state = license::verify_license_with_key(&on_disk, &pk).expect("trailing newline tolerated");
    assert_eq!(state.org, "test");
}

/// Days since 1970-01-01 → civil date (Hinnant's algorithm — the inverse of
/// the verifier's days_from_civil).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}

/// The UTC date `n` days before today, as "YYYY-MM-DD".
fn date_days_ago(n: i64) -> String {
    let now_days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        / 86_400;
    let (y, m, d) = civil_from_days(now_days - n);
    format!("{y:04}-{m:02}-{d:02}")
}

#[test]
fn expired_beyond_grace_rejected() {
    // Expiry is END of day: 4 days back puts it 72–96h in the past — always
    // beyond the 72h grace, whatever time of day this test runs.
    let (pk, blob) = sign("test", Some(date_days_ago(4)), &["all"]);
    let err = license::verify_license_with_key(&blob, &pk).unwrap_err();
    assert!(matches!(err, license::LicenseError::Expired), "got {err:?}");
}

#[test]
fn expiry_grace_window() {
    // 2 days back puts end-of-day expiry 24–48h in the past: expired, but
    // within the 72h grace — the license still verifies.
    let (pk, blob) = sign("test", Some(date_days_ago(2)), &["all"]);
    let state = license::verify_license_with_key(&blob, &pk).expect("grace license");
    assert_eq!(state.org, "test");
    assert!(state.expires.is_some());
}

#[test]
fn oidc_sso_implies_multi_user() {
    let f = license::FeatureSet::from_flags(&["oidc-sso".to_string()]);
    assert!(f.oidc_sso && f.multi_user && !f.audit_export);
}

#[test]
fn production_public_key_is_a_valid_ed25519_key() {
    let pk = URL_SAFE_NO_PAD
        .decode(license::LICENSE_PUBLIC_KEY_B64)
        .expect("the pinned public key is base64url");
    assert_eq!(pk.len(), 32, "ed25519 public keys are 32 bytes");
}

// --- startup degradation (spawns the aikoql-mcp binary) ---

/// A license signed by a TEST keypair — the production key rejects it — must
/// degrade to community mode: the server still starts, /health responds, and
/// the reason is logged.
#[test]
fn tampered_key_runs_as_community() {
    let (_, mut blob) = sign("test", None, &["all"]);
    tamper_mid_segment(&mut blob, 1);
    let lic = std::env::temp_dir().join(format!("aikoql-license-{}.lic", std::process::id()));
    std::fs::write(&lic, &blob).unwrap();
    common::register_temp_path(&lic);
    let db = std::env::temp_dir().join(format!("aikoql-license-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-license-log-{}.log", std::process::id()));
    common::register_temp_path(&log);

    let lic_str = lic.to_str().unwrap();
    let srv = common::spawn_server(&db, &log, &[("AIKOQL_LICENSE", lic_str)], &[]);

    // /health comes up — community mode still serves
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let health = common::wait_health(srv.port, deadline);
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );

    // the rejection is logged with the reason, naming community mode
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    common::wait_log(&log, "community mode", deadline);
}

#[test]
fn expiry_date_renders_the_expiry_day() {
    // Task 9: expires is the END of the expiry day (00:00 UTC of the next
    // one) — the license line must show the day the owner wrote in
    // expires_at, as civil_from_days here would.
    let end = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(4_102_444_800); // 2100-01-01T00:00Z
    assert_eq!(license::expiry_date(end), "2099-12-31");
    let epoch_next = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(86_400); // 1970-01-02T00:00Z
    assert_eq!(license::expiry_date(epoch_next), "1970-01-01");
}
