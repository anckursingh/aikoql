//! Offline enterprise license verification (Task 2).
//!
//! Envelope: `base64url(json_payload) "." base64url(ed25519_signature)`,
//! URL_SAFE alphabet, no padding — exactly what the owner-side keygen
//! (scripts/license-keygen) signs. The signature must verify against
//! `LICENSE_PUBLIC_KEY_B64` BEFORE the payload is parsed; any malformation
//! is an error, never a default license (fail closed).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use std::time::{Duration, SystemTime};

/// The production ed25519 public key (base64url, no padding), generated
/// with `scripts/license-keygen keygen` (Task 1); the private half is
/// owner custody, off-repo.
pub(crate) const LICENSE_PUBLIC_KEY_B64: &str = "8W3MHcAeYRwLYiLYmJZDpim66gGwigRXEY_nTzebrHA";

/// The wire `ver` this server accepts (the keygen emits 1).
const LICENSE_VERSION: u32 = 1;

/// Expired licenses stay valid for 72h past their end-of-day expiry.
const GRACE: Duration = Duration::from_secs(72 * 3600);

/// The process license (set at startup in main.rs); unset = community mode.
/// Tasks 5/7/8/9 read it to gate the enterprise features.
pub(crate) static LICENSE_STATE: std::sync::OnceLock<LicenseState> = std::sync::OnceLock::new();

/// A verified license's payload.
#[derive(Debug, Clone)]
pub(crate) struct LicenseState {
    pub org: String,
    /// End of the expiry day, UTC; None = perpetual.
    #[allow(dead_code)] // the Tasks 5/7/8/9 contract — read there
    pub expires: Option<SystemTime>,
    #[allow(dead_code)] // the Tasks 5/7/8/9 contract — read there
    pub features: FeatureSet,
}

/// Enterprise feature flags gated by the license.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FeatureSet {
    pub oidc_sso: bool,
    pub multi_user: bool,
    pub audit_export: bool,
}

impl FeatureSet {
    /// The wire `features` list → flags. `"all"` arms everything; an
    /// oidc-sso license implies multi-user; unknown flags are ignored (a
    /// newer keygen's feature never breaks an older server).
    pub(crate) fn from_flags(flags: &[String]) -> FeatureSet {
        let mut f = FeatureSet::default();
        for flag in flags {
            match flag.as_str() {
                "all" => {
                    f.oidc_sso = true;
                    f.multi_user = true;
                    f.audit_export = true;
                }
                "oidc-sso" => {
                    f.oidc_sso = true;
                    f.multi_user = true; // oidc-sso ⇒ multi-user
                }
                "multi-user" => f.multi_user = true,
                "audit-export" => f.audit_export = true,
                _ => {}
            }
        }
        f
    }
}

/// The set flags as wire names, in the order the /api/license payload
/// lists them — the status line and the payload share this single list.
pub(crate) fn feature_flags(f: &FeatureSet) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if f.multi_user {
        flags.push("multi-user");
    }
    if f.oidc_sso {
        flags.push("oidc-sso");
    }
    if f.audit_export {
        flags.push("audit-export");
    }
    flags
}

/// Why a license was rejected — Display goes into the startup warn.
#[derive(Debug)]
pub(crate) enum LicenseError {
    /// Not UTF-8, missing the `.` separator, or an empty segment.
    Malformed(&'static str),
    /// A segment is not base64url.
    Base64(base64::DecodeError),
    /// The signature does not verify against the public key.
    Signature,
    /// The payload is not the wire JSON object.
    Json(serde_json::Error),
    /// A field violates the wire contract (ver, issued_at, expires_at).
    Fields(&'static str),
    /// Expired beyond the 72h grace window.
    Expired,
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LicenseError::Malformed(m) => write!(f, "malformed envelope: {m}"),
            LicenseError::Base64(e) => write!(f, "invalid base64: {e}"),
            LicenseError::Signature => write!(f, "signature does not verify"),
            LicenseError::Json(e) => write!(f, "payload: {e}"),
            LicenseError::Fields(m) => write!(f, "invalid field: {m}"),
            LicenseError::Expired => write!(f, "expired beyond the 72h grace window"),
        }
    }
}

/// The wire payload (Task 1's keygen): timestamps are strings on the wire —
/// `issued_at` is unix seconds, `expires_at` is YYYY-MM-DD or null.
#[derive(serde::Deserialize)]
struct WireLicense {
    ver: u32,
    org: String,
    issued_at: String,
    expires_at: Option<String>,
    features: Vec<String>,
}

/// The public key `verify_license` pins: the embedded production key — or,
/// in DEBUG builds only, `AIKOQL_LICENSE_PUBLIC_KEY` (base64url, no
/// padding) when set, so the integration tests can spawn a debug server
/// licensed by a TEST keypair. Release builds never read the env: a
/// shipped binary always verifies against the production key.
#[cfg(debug_assertions)]
fn active_public_key_b64() -> String {
    std::env::var("AIKOQL_LICENSE_PUBLIC_KEY")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| LICENSE_PUBLIC_KEY_B64.to_string())
}

#[cfg(not(debug_assertions))]
fn active_public_key_b64() -> &'static str {
    LICENSE_PUBLIC_KEY_B64
}

/// Verify an envelope against the production public key — or, in debug
/// builds, the `AIKOQL_LICENSE_PUBLIC_KEY` override (see
/// `active_public_key_b64`). Fail closed: any malformation — envelope,
/// base64, signature, fields, or an expiry beyond the grace window — is an
/// Err, never a default license.
pub(crate) fn verify_license(bytes: &[u8]) -> Result<LicenseState, LicenseError> {
    // a malformed override falls back to the production key — a test
    // fixture then fails verification and degrades to community mode
    let pk = URL_SAFE_NO_PAD
        .decode(active_public_key_b64())
        .unwrap_or_else(|_| {
            URL_SAFE_NO_PAD
                .decode(LICENSE_PUBLIC_KEY_B64)
                .expect("LICENSE_PUBLIC_KEY_B64 is a fixed, valid base64url public key")
        });
    verify_license_with_key(bytes, &pk)
}

/// The same verification against an explicit key. Production goes through
/// `verify_license`; the tests sign fixtures with their own keypair and
/// verify against its public half (never the production key).
pub(crate) fn verify_license_with_key(
    bytes: &[u8],
    pk: &[u8],
) -> Result<LicenseState, LicenseError> {
    // a license file may carry surrounding whitespace (the keygen itself
    // prints a trailing newline) — trim before decoding
    let text = std::str::from_utf8(bytes.trim_ascii()).map_err(|_| LicenseError::Malformed("not UTF-8"))?;
    // split on the LAST '.' — a stray separator in either half fails base64
    let (payload_b64, sig_b64) = text
        .rsplit_once('.')
        .ok_or(LicenseError::Malformed("expected payload.signature"))?;
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(LicenseError::Malformed(
            "empty payload or signature segment",
        ));
    }
    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(LicenseError::Base64)?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(LicenseError::Base64)?;
    // the signature gate comes before the parse: only owner-signed bytes
    // are ever interpreted
    UnparsedPublicKey::new(&ED25519, pk)
        .verify(&payload, &sig)
        .map_err(|_| LicenseError::Signature)?;
    let wire: WireLicense = serde_json::from_slice(&payload).map_err(LicenseError::Json)?;
    if wire.ver != LICENSE_VERSION {
        return Err(LicenseError::Fields("unknown ver"));
    }
    wire.issued_at
        .parse::<u64>()
        .map_err(|_| LicenseError::Fields("issued_at is not unix seconds"))?;
    let expires = wire
        .expires_at
        .as_deref()
        .map(expiry_system_time)
        .transpose()?;
    Ok(LicenseState {
        org: wire.org,
        expires,
        features: FeatureSet::from_flags(&wire.features),
    })
}

/// "YYYY-MM-DD" → the end of that day in UTC (00:00 of the next day),
/// checked against now + the 72h grace. Civil-date arithmetic via
/// Hinnant's days_from_civil — no chrono dependency for one conversion.
fn expiry_system_time(date: &str) -> Result<SystemTime, LicenseError> {
    const BAD_DATE: &str = "expires_at is not YYYY-MM-DD";
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(LicenseError::Fields(BAD_DATE));
    }
    let part = |r: std::ops::Range<usize>| -> Result<i64, LicenseError> {
        if !b[r.clone()].iter().all(u8::is_ascii_digit) {
            return Err(LicenseError::Fields(BAD_DATE));
        }
        // justified: at most 4 ASCII digits — parse cannot fail
        Ok(std::str::from_utf8(&b[r]).unwrap().parse().unwrap())
    };
    let (y, m, d) = (part(0..4)?, part(5..7)?, part(8..10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(LicenseError::Fields(BAD_DATE));
    }
    let end_of_day = SystemTime::UNIX_EPOCH
        + Duration::from_secs(((days_from_civil(y, m, d) + 1) * 86_400).max(0) as u64);
    if let Ok(age) = SystemTime::now().duration_since(end_of_day) {
        if age > GRACE {
            return Err(LicenseError::Expired);
        }
        tracing::warn!(
            hours = age.as_secs() / 3600,
            "license expired — within the 72h grace window"
        );
    }
    Ok(end_of_day)
}

/// "YYYY-MM-DD" for the expiry instant — expires is the END of the expiry
/// day (00:00 UTC of the next one), so the visible date is one second back.
pub(crate) fn expiry_date(expires: SystemTime) -> String {
    let secs = expires
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs.saturating_sub(1) / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 → civil date (Hinnant's algorithm) — the inverse
/// of days_from_civil.
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

/// Days since 1970-01-01 for a civil date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}
