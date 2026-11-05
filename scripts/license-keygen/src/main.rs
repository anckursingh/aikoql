//! Owner-side aikoql license tool: `keygen` mints an ed25519 pair, `sign`
//! emits the license envelope the server verifies offline.
//!
//! Envelope: base64url(json payload) "." base64url(ed25519 signature),
//! URL_SAFE, no padding. Owner custody only — never shipped in a release.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
#[cfg(test)]
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Serialize, Serializer};

const USAGE: &str = "\
usage:
  license-keygen keygen
  license-keygen sign --org ORG [--expires YYYY-MM-DD] --features all|oidc-sso,multi-user,audit-export

sign reads the private key from AIKOQL_LICENSE_SK (the SK= line from keygen).";

/// Wire payload parsed by the server's verifier. Timestamps are strings on
/// the wire: `issued_at` is unix seconds, `expires_at` is the owner's
/// YYYY-MM-DD (None = perpetual).
#[derive(Serialize)]
struct LicensePayload {
    ver: u32,
    org: String,
    #[serde(serialize_with = "serialize_u64_as_str")]
    issued_at: u64,
    expires_at: Option<String>,
    features: Vec<String>,
}

// Tests and the CLI build the payload from a unix-seconds integer; the wire
// type stays a string for the server's verifier.
fn serialize_u64_as_str<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

/// Fresh pair as base64url(no pad): PKCS#8 private key, raw public key.
fn generate_pair() -> (String, String) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("system RNG");
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("fresh PKCS#8");
    (
        URL_SAFE_NO_PAD.encode(pkcs8.as_ref()),
        URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    )
}

/// Signs `payload` with the base64 PKCS#8 key and returns the envelope bytes.
/// Panics on a malformed key — owner-side tool, fail loudly.
fn sign_license(sk_b64: &str, payload: &LicensePayload) -> Box<[u8]> {
    let pkcs8 = URL_SAFE_NO_PAD.decode(sk_b64).expect("SK is not base64url");
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).expect("SK is not a PKCS#8 ed25519 key");
    let json = serde_json::to_vec(payload).expect("serialize payload");
    let sig = pair.sign(&json);
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&json),
        URL_SAFE_NO_PAD.encode(sig.as_ref())
    )
    .into_bytes()
    .into_boxed_slice()
}

#[cfg(test)]
fn verify_license_blob(pk_b64: &str, blob: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(blob).map_err(|_| "blob is not UTF-8".to_string())?;
    let (payload_b64, sig_b64) = text.split_once('.').ok_or("missing '.' separator")?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|e| format!("payload: {e}"))?;
    let sig = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|e| format!("signature: {e}"))?;
    let pk = URL_SAFE_NO_PAD.decode(pk_b64).map_err(|e| format!("public key: {e}"))?;
    UnparsedPublicKey::new(&ED25519, pk)
        .verify(&payload, &sig)
        .map_err(|_| "signature does not verify".to_string())
}

const FEATURES: [&str; 3] = ["oidc-sso", "multi-user", "audit-export"];

fn parse_features(raw: &str) -> Result<Vec<String>, String> {
    if raw == "all" {
        return Ok(vec!["all".to_string()]);
    }
    let mut features = Vec::new();
    for f in raw.split(',') {
        if !FEATURES.contains(&f) {
            return Err(format!(
                "unknown feature {f:?}; expected all or a comma list of {FEATURES:?}"
            ));
        }
        features.push(f.to_string());
    }
    Ok(features)
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn sign_cmd(args: &[String]) -> Result<(), String> {
    let (mut org, mut expires, mut features) = (None, None, None);
    let mut i = 0;
    while i < args.len() {
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{} needs a value", args[i]))?;
        match args[i].as_str() {
            "--org" => org = Some(value.clone()),
            "--expires" => expires = Some(value.clone()),
            "--features" => features = Some(value.clone()),
            other => return Err(format!("unknown argument {other:?}")),
        }
        i += 2;
    }
    let org = org.ok_or("--org is required")?;
    let features = parse_features(features.as_deref().ok_or("--features is required")?)?;
    if let Some(date) = &expires {
        if !is_date(date) {
            return Err(format!("--expires must be YYYY-MM-DD, got {date:?}"));
        }
    }
    let sk = std::env::var("AIKOQL_LICENSE_SK")
        .map_err(|_| "AIKOQL_LICENSE_SK is not set (use the SK= line from keygen)".to_string())?;
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the unix epoch".to_string())?
        .as_secs();
    let payload = LicensePayload {
        ver: 1,
        org,
        issued_at,
        expires_at: expires,
        features,
    };
    let blob = sign_license(&sk, &payload);
    println!(
        "{}",
        String::from_utf8(blob.into_vec()).expect("envelope is ASCII")
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args.first().map(String::as_str) {
        Some("keygen") => {
            let (sk, pk) = generate_pair();
            println!("SK={sk}");
            println!("PK={pk}");
            Ok(())
        }
        Some("sign") => sign_cmd(&args[1..]),
        _ => Err("expected keygen or sign".to_string()),
    };
    if let Err(e) = outcome {
        eprintln!("error: {e}\n\n{USAGE}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_roundtrip_sign_then_verify() {
        let (sk, pk) = generate_pair();
        let blob = sign_license(&sk, &LicensePayload { ver: 1, org: "test".into(), issued_at: 0, expires_at: None, features: vec!["all".into()] });
        assert!(verify_license_blob(&pk, &blob).is_ok());
        let mut tampered = blob.clone(); tampered[tampered.len() - 1] ^= 1;
        assert!(verify_license_blob(&pk, &tampered).is_err());
    }
}
