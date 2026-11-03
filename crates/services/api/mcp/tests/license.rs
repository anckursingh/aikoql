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

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::time::{SystemTime, UNIX_EPOCH};

/// Sign a payload with a fresh test keypair → (public key bytes, envelope).
fn sign(org: &str, expires: Option<String>, features: &[&str]) -> (Vec<u8>, Vec<u8>) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("system RNG");
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("fresh PKCS#8");
    let payload = serde_json::json!({
        "ver": 1,
        "org": org,
        "issued_at": "0",
        "expires_at": expires,
        "features": features,
    });
    let json = serde_json::to_vec(&payload).unwrap();
    let sig = pair.sign(&json);
    let envelope = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&json),
        URL_SAFE_NO_PAD.encode(sig.as_ref())
    );
    (pair.public_key().as_ref().to_vec(), envelope.into_bytes())
}

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

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};

mod common;

/// Hard-kill a still-running server child on drop (a panicked test must not
/// leak the process).
struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// One GET over plain TCP; the server closes after the response.
fn http_get(port: u16) -> Result<String, String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("read timeout");
    s.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    s.flush().unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out); // EOF = the response is complete
    Ok(String::from_utf8_lossy(&out).into_owned())
}

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

    let port = free_port();
    let _srv = ServerGuard(
        Command::new(env!("CARGO_BIN_EXE_aikoql-mcp"))
            .arg("serve")
            .arg("--metrics-addr")
            .arg(format!("127.0.0.1:{port}"))
            .arg(&db)
            .env("AIKOQL_LICENSE", &lic)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()))
            .spawn()
            .expect("spawn aikoql-mcp serve"),
    );

    // /health comes up — community mode still serves
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let health = loop {
        if let Ok(resp) = http_get(port) {
            break resp;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "server never served /health"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(
        health.starts_with("HTTP/1.") && health.contains(" 200 "),
        "health: {health}"
    );

    // the rejection is logged with the reason, naming community mode
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let log_text = std::fs::read_to_string(&log).unwrap_or_default();
        if log_text.contains("community mode") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no community-mode log: {log_text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
