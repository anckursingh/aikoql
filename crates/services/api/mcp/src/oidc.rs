//! Hand-rolled OIDC client (Task 4): authorization-code + PKCE S256, with
//! id_token verification (RS256) against the provider's JWKS. Blocking
//! ureq calls — the binary carries no general async runtime (see s3.rs).
//! Task 5 wires `OidcProvider` into the Studio login and the MCP
//! /oauth/authorize redirect.
#![allow(dead_code)] // every pub(crate) item below is the Task 5 contract — dead until wired there

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{
    RsaKeyPair, RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256,
};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The OIDC surface Task 5 consumes: build the IdP authorization URL, then
/// exchange the callback code for verified id-token claims.
pub(crate) trait OidcProvider {
    fn authorize_url(&self, state: &str, verifier: &str) -> Result<String, OidcError>;
    fn exchange(&self, code: &str, verifier: &str) -> Result<IdTokenClaims, OidcError>;
}

/// The verified id_token's claims — everything the enterprise login needs.
#[derive(Debug)]
pub(crate) struct IdTokenClaims {
    pub sub: String,
    pub name: Option<String>,
    /// The roles claim (`AIKOQL_OIDC_ROLES_CLAIM`, default "roles"); absent
    /// → empty — the caller maps empty to viewer.
    pub roles: Vec<String>,
}

/// Why an OIDC step failed — Display names exactly what failed.
#[derive(Debug)]
pub(crate) enum OidcError {
    /// A required env var is unset.
    Config(&'static str),
    /// An HTTP call failed (context names the call).
    Http {
        context: &'static str,
        status: Option<u16>,
        body: String,
    },
    /// An HTTP transport failure (context names the call).
    Transport { context: &'static str, what: String },
    /// A JSON document (discovery, JWKS, token header/payload) failed to
    /// parse.
    Json {
        context: &'static str,
        error: String,
    },
    /// A required discovery/JWKS/token field is missing or mistyped.
    Missing(String),
    /// The id_token is not header.payload.signature.
    Malformed(&'static str),
    /// A token segment is not base64url.
    Base64(base64::DecodeError),
    /// No JWKS key verifies the signature.
    Signature,
    /// The token header `alg` is not RS256.
    UnsupportedAlg(String),
    /// iss/aud/exp/sub violated the configured contract.
    Claims(&'static str),
    /// Key material failed to load.
    Key(&'static str),
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OidcError::Config(v) => write!(f, "OIDC config: {v} is not set"),
            OidcError::Http {
                context,
                status,
                body,
            } => match status {
                Some(s) => write!(f, "OIDC {context}: HTTP {s}: {body}"),
                None => write!(f, "OIDC {context}: {body}"),
            },
            OidcError::Transport { context, what } => {
                write!(f, "OIDC {context} transport: {what}")
            }
            OidcError::Json { context, error } => write!(f, "OIDC {context} JSON: {error}"),
            OidcError::Missing(m) => write!(f, "OIDC {m}"),
            OidcError::Malformed(m) => write!(f, "OIDC id_token malformed: {m}"),
            OidcError::Base64(e) => write!(f, "OIDC id_token base64: {e}"),
            OidcError::Signature => write!(f, "OIDC id_token signature does not verify"),
            OidcError::UnsupportedAlg(a) => write!(f, "OIDC id_token alg {a} unsupported (RS256)"),
            OidcError::Claims(m) => write!(f, "OIDC id_token claims: {m}"),
            OidcError::Key(m) => write!(f, "OIDC key: {m}"),
        }
    }
}

/// One RSA JWKS key: the modulus and exponent as big-endian bytes — exactly
/// what ring's `RsaPublicKeyComponents` verifies with (no DER needed).
struct RsaKey {
    kid: Option<String>,
    n: Vec<u8>,
    e: Vec<u8>,
}

/// The real IdP client: discovery + JWKS at construction, then
/// `authorize_url` / `exchange`. Config: `AIKOQL_OIDC_ISSUER` (required —
/// `from_env` errs when unset, which is how Task 5 detects "no OIDC
/// configured"), `AIKOQL_OIDC_CLIENT_ID`, `AIKOQL_OIDC_CLIENT_SECRET`,
/// `AIKOQL_OIDC_REDIRECT_URI` (all required), `AIKOQL_OIDC_ROLES_CLAIM`
/// (default "roles").
pub(crate) struct HttpOidcProvider {
    issuer: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    roles_claim: String,
    authorization_endpoint: String,
    token_endpoint: String,
    keys: Vec<RsaKey>,
    agent: ureq::Agent,
}

impl HttpOidcProvider {
    pub(crate) fn from_env() -> Result<Self, OidcError> {
        let env = |k: &'static str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let need = |k: &'static str| env(k).ok_or(OidcError::Config(k));
        let issuer = need("AIKOQL_OIDC_ISSUER")?
            .trim_end_matches('/')
            .to_string();
        let client_id = need("AIKOQL_OIDC_CLIENT_ID")?;
        let client_secret = need("AIKOQL_OIDC_CLIENT_SECRET")?;
        let redirect_uri = need("AIKOQL_OIDC_REDIRECT_URI")?;
        let roles_claim = env("AIKOQL_OIDC_ROLES_CLAIM").unwrap_or_else(|| "roles".into());
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build();
        let discovery = get_json(
            &agent,
            &format!("{issuer}/.well-known/openid-configuration"),
            "discovery",
        )?;
        let authorization_endpoint =
            str_field(&discovery, "authorization_endpoint", "discovery")?.to_string();
        let token_endpoint = str_field(&discovery, "token_endpoint", "discovery")?.to_string();
        let jwks_uri = str_field(&discovery, "jwks_uri", "discovery")?;
        if str_field(&discovery, "issuer", "discovery")? != issuer {
            return Err(OidcError::Claims(
                "discovery issuer does not match AIKOQL_OIDC_ISSUER",
            ));
        }
        let jwks = get_json(&agent, jwks_uri, "jwks")?;
        let mut keys = Vec::new();
        for key in jwks
            .get("keys")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if key.get("kty").and_then(|v| v.as_str()) != Some("RSA") {
                continue;
            }
            if let Some(alg) = key.get("alg").and_then(|v| v.as_str()) {
                if alg != "RS256" {
                    continue;
                }
            }
            let decode = |f: &str| -> Result<Vec<u8>, OidcError> {
                let b64 = key
                    .get(f)
                    .and_then(|v| v.as_str())
                    .ok_or(OidcError::Missing("a JWKS RSA key has no n/e".into()))?;
                URL_SAFE_NO_PAD.decode(b64).map_err(OidcError::Base64)
            };
            keys.push(RsaKey {
                kid: key.get("kid").and_then(|v| v.as_str()).map(str::to_string),
                n: decode("n")?,
                e: decode("e")?,
            });
        }
        if keys.is_empty() {
            return Err(OidcError::Missing("the JWKS has no RS256 RSA keys".into()));
        }
        Ok(Self {
            issuer,
            client_id,
            client_secret,
            redirect_uri,
            roles_claim,
            authorization_endpoint,
            token_endpoint,
            keys,
            agent,
        })
    }
}

impl OidcProvider for HttpOidcProvider {
    fn authorize_url(&self, state: &str, verifier: &str) -> Result<String, OidcError> {
        Ok(format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope=openid&state={}&code_challenge={}&code_challenge_method=S256",
            self.authorization_endpoint,
            form_encode(&self.client_id),
            form_encode(&self.redirect_uri),
            form_encode(state),
            pkce_challenge(verifier),
        ))
    }

    /// RFC 6749 client_secret_basic: Basic base64(client_id:client_secret),
    /// form-encoded body, PKCE verifier.
    fn exchange(&self, code: &str, verifier: &str) -> Result<IdTokenClaims, OidcError> {
        let auth = format!(
            "Basic {}",
            STANDARD.encode(format!("{}:{}", self.client_id, self.client_secret))
        );
        let body = format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}",
            form_encode(code),
            form_encode(&self.redirect_uri),
            form_encode(verifier),
        );
        let resp = self
            .agent
            .post(&self.token_endpoint)
            .set("authorization", &auth)
            .set("content-type", "application/x-www-form-urlencoded")
            .send_string(&body)
            .map_err(|e| http_error("token", e))?;
        let json: serde_json::Value = resp.into_json().map_err(|e| OidcError::Json {
            context: "token response",
            error: e.to_string(),
        })?;
        let id_token = json
            .get("id_token")
            .and_then(|v| v.as_str())
            .ok_or(OidcError::Missing(
                "the token response has no id_token".into(),
            ))?;
        verify_id_token(
            id_token,
            &self.keys,
            &self.issuer,
            &self.client_id,
            &self.roles_claim,
        )
    }
}

/// The in-process IdP stand-in the tests (and Task 5's SSO integration
/// tests) use so a real IdP is never needed: it signs id_tokens with the
/// committed fixture key and verifies them through the same
/// [`verify_id_token`] path as the HTTP provider.
pub(crate) struct MockOidcProvider {
    issuer: String,
    client_id: String,
    claims: MockClaims,
    key: RsaKeyPair,
}

/// What every token the mock signs carries.
pub(crate) struct MockClaims {
    pub sub: String,
    pub name: Option<String>,
    /// None → the token carries no roles claim at all (the verifier then
    /// yields an empty roles vec).
    pub roles: Option<Vec<String>>,
}

impl MockOidcProvider {
    /// `pkcs8_pem` is the fixture key's PKCS#8 PEM text
    /// (tests/fixtures/oidc-test-rsa.pem).
    pub(crate) fn new(issuer: &str, client_id: &str, pkcs8_pem: &str) -> Result<Self, OidcError> {
        let key = RsaKeyPair::from_pkcs8(&pem_body(pkcs8_pem)?)
            .map_err(|_| OidcError::Key("not a PKCS#8 RSA private key"))?;
        Ok(Self {
            issuer: issuer.to_string(),
            client_id: client_id.to_string(),
            claims: MockClaims {
                sub: "test-user".into(),
                name: Some("Test User".into()),
                roles: Some(vec!["viewer".into()]),
            },
            key,
        })
    }

    /// Override the claims the mock signs (Task 5's tests: unknown roles,
    /// missing roles, …).
    pub(crate) fn with_claims(mut self, claims: MockClaims) -> Self {
        self.claims = claims;
        self
    }

    /// A freshly signed id_token for the configured claims.
    fn token(&self) -> Result<String, OidcError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut payload = serde_json::json!({
            "iss": self.issuer,
            "sub": self.claims.sub,
            "aud": self.client_id,
            "exp": now + 300,
        });
        if let Some(name) = &self.claims.name {
            payload["name"] = serde_json::Value::String(name.clone());
        }
        if let Some(roles) = &self.claims.roles {
            payload["roles"] = serde_json::json!(roles);
        }
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(
                    &serde_json::json!({"alg": "RS256", "kid": "mock", "typ": "JWT"})
                )
                .expect("header JSON")
            ),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload JSON")),
        );
        let mut signature = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut signature,
            )
            .map_err(|_| OidcError::Key("signing failed"))?;
        Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    /// The mock's key as JWKS-shaped entries — so the mock verifies through
    /// the same key-selection path the HTTP provider uses.
    fn keys(&self) -> Vec<RsaKey> {
        let components = RsaPublicKeyComponents::<Vec<u8>>::from(self.key.public());
        vec![RsaKey {
            kid: Some("mock".into()),
            n: components.n,
            e: components.e,
        }]
    }
}

impl OidcProvider for MockOidcProvider {
    fn authorize_url(&self, state: &str, verifier: &str) -> Result<String, OidcError> {
        // a placeholder URL — Task 5's tests follow the redirect target, not
        // this host (.invalid never resolves)
        Ok(format!(
            "https://oidc.test.invalid/authorize?response_type=code&client_id={}&redirect_uri={}&scope=openid&state={}&code_challenge={}&code_challenge_method=S256",
            form_encode(&self.client_id),
            form_encode("http://127.0.0.1:1/oidc/callback"),
            form_encode(state),
            pkce_challenge(verifier),
        ))
    }

    /// Any code exchanges — the claims are what the test configured.
    fn exchange(&self, _code: &str, _verifier: &str) -> Result<IdTokenClaims, OidcError> {
        verify_id_token(
            &self.token()?,
            &self.keys(),
            &self.issuer,
            &self.client_id,
            "roles",
        )
    }
}

/// Verify an id_token end to end: structure, RS256 signature against the
/// JWKS keys, then `iss` (must equal `issuer`), `aud` (must contain
/// `client_id`), `exp` (must be in the future), `sub` (required), plus the
/// optional `name` and the roles claim (missing → empty).
fn verify_id_token(
    token: &str,
    keys: &[RsaKey],
    issuer: &str,
    client_id: &str,
    roles_claim: &str,
) -> Result<IdTokenClaims, OidcError> {
    let mut parts = token.split('.');
    let (header_b64, payload_b64, sig_b64) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if parts.next().is_none() => (h, p, s),
        _ => return Err(OidcError::Malformed("expected header.payload.signature")),
    };
    let decode_json = |segment: &str, what: &'static str| -> Result<serde_json::Value, OidcError> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).map_err(OidcError::Base64)?)
            .map_err(|error| OidcError::Json {
                context: what,
                error: error.to_string(),
            })
    };
    let header = decode_json(header_b64, "id_token header")?;
    match header.get("alg").and_then(|v| v.as_str()) {
        Some("RS256") => {}
        Some(other) => return Err(OidcError::UnsupportedAlg(other.to_string())),
        None => return Err(OidcError::Malformed("the header has no alg")),
    }
    let kid = header.get("kid").and_then(|v| v.as_str());
    let signature = URL_SAFE_NO_PAD.decode(sig_b64).map_err(OidcError::Base64)?;
    let signed = format!("{header_b64}.{payload_b64}");
    // the kid's key first (rotation-safe), then any other key
    let mut candidates: Vec<&RsaKey> = keys.iter().collect();
    if let Some(k) = kid {
        candidates.sort_by_key(|c| if c.kid.as_deref() == Some(k) { 0 } else { 1 });
    }
    if candidates.is_empty() {
        return Err(OidcError::Key("no JWKS keys"));
    }
    if !candidates.iter().any(|c| {
        RsaPublicKeyComponents {
            n: c.n.as_slice(),
            e: c.e.as_slice(),
        }
        .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
        .is_ok()
    }) {
        return Err(OidcError::Signature);
    }
    let payload = decode_json(payload_b64, "id_token payload")?;
    if payload.get("iss").and_then(|v| v.as_str()) != Some(issuer) {
        return Err(OidcError::Claims("iss does not match the issuer"));
    }
    let aud_ok = match payload.get("aud") {
        Some(serde_json::Value::String(s)) => s == client_id,
        Some(serde_json::Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err(OidcError::Claims("aud does not contain the client_id"));
    }
    let exp = payload
        .get("exp")
        .and_then(|v| v.as_u64())
        .ok_or(OidcError::Claims("exp is not unix seconds"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if exp <= now {
        return Err(OidcError::Claims("expired"));
    }
    let sub = payload
        .get("sub")
        .and_then(|v| v.as_str())
        .ok_or(OidcError::Claims("sub is missing"))?
        .to_string();
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let roles = match payload.get(roles_claim) {
        None => Vec::new(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or(OidcError::Claims("the roles claim is not a string array"))?,
        Some(_) => return Err(OidcError::Claims("the roles claim is not a string array")),
    };
    Ok(IdTokenClaims { sub, name, roles })
}

/// 32 random bytes, base64url, no padding — the PKCE S256 verifier.
/// Task 5 generates one per authorize redirect.
pub(crate) fn pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    // the OS RNG failing is unrecoverable — panic like any OOM
    getrandom::getrandom(&mut bytes).expect("OS RNG");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The PKCE S256 challenge for a verifier: base64url(sha256(verifier)).
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// application/x-www-form-urlencoded: unreserved stays, space → '+'.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The DER body of a PEM (the base64 between the BEGIN/END lines).
fn pem_body(pem: &str) -> Result<Vec<u8>, OidcError> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    STANDARD.decode(body).map_err(OidcError::Base64)
}

/// ureq's error is too large to carry around — (status, first 200 chars),
/// like s3.rs's `describe`.
fn http_error(context: &'static str, e: ureq::Error) -> OidcError {
    match e {
        ureq::Error::Status(code, resp) => {
            let body: String = resp.into_string().unwrap_or_default();
            let body: String = body.chars().take(200).collect();
            OidcError::Http {
                context,
                status: Some(code),
                body,
            }
        }
        ureq::Error::Transport(t) => OidcError::Transport {
            context,
            what: t.to_string(),
        },
    }
}

fn get_json(
    agent: &ureq::Agent,
    url: &str,
    context: &'static str,
) -> Result<serde_json::Value, OidcError> {
    agent
        .get(url)
        .call()
        .map_err(|e| http_error(context, e))?
        .into_json()
        .map_err(|error| OidcError::Json {
            context,
            error: error.to_string(),
        })
}

fn str_field<'a>(
    doc: &'a serde_json::Value,
    field: &str,
    context: &'static str,
) -> Result<&'a str, OidcError> {
    doc.get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| OidcError::Missing(format!("{context} has no {field}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUER: &str = "https://idp.test.invalid";
    const CLIENT_ID: &str = "aikoql-test-client";

    /// The committed fixture key (PKCS#8 PEM, 2048-bit RSA) — test-only,
    /// see tests/fixtures/README.md.
    fn mock() -> MockOidcProvider {
        let pem = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/oidc-test-rsa.pem"
        ))
        .expect("fixture PEM present");
        MockOidcProvider::new(ISSUER, CLIENT_ID, &pem).expect("fixture key parses")
    }

    /// The mock signs an id_token with sub/name/roles; exchange verifies it
    /// through the real RS256 + claims path.
    #[test]
    fn exchange_valid_token_parses_claims() {
        let provider = mock();
        let claims = provider
            .exchange("code-1", &pkce_verifier())
            .expect("mock exchange verifies");
        assert_eq!(claims.sub, "test-user");
        assert_eq!(claims.name.as_deref(), Some("Test User"));
        assert_eq!(claims.roles, vec!["viewer".to_string()]);
    }

    /// One flipped byte mid-signature (never the last byte of the string)
    /// must fail RS256 verification.
    #[test]
    fn rs256_bad_signature_rejected() {
        let provider = mock();
        let token = provider.token().expect("mock signs");
        let sig_start = token.rfind('.').expect("signature segment") + 1;
        let mut bytes = token.into_bytes();
        // always a valid base64url char, always a different byte
        bytes[sig_start + 10] = if bytes[sig_start + 10] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let tampered = String::from_utf8(bytes).expect("flip stays ASCII");
        assert!(matches!(
            verify_id_token(&tampered, &provider.keys(), ISSUER, CLIENT_ID, "roles"),
            Err(OidcError::Signature)
        ));
    }

    /// No roles claim in the token → empty vec (the caller maps that to
    /// viewer).
    #[test]
    fn roles_claim_missing_yields_empty() {
        let provider = mock().with_claims(MockClaims {
            sub: "no-roles".into(),
            name: None,
            roles: None,
        });
        let claims = provider
            .exchange("code-2", &pkce_verifier())
            .expect("mock exchange verifies");
        assert_eq!(claims.sub, "no-roles");
        assert!(claims.roles.is_empty());
    }

    /// The S256 round-trip: the code_challenge the mock embeds in its
    /// authorize URL equals base64url(sha256(verifier)).
    #[test]
    fn pkce_verifier_matches_s256() {
        let provider = mock();
        let verifier = pkce_verifier();
        let url = provider
            .authorize_url("state-xyz", &verifier)
            .expect("authorize url");
        let challenge = url
            .split('&')
            .find_map(|pair| pair.strip_prefix("code_challenge="))
            .expect("code_challenge in the URL")
            .to_string();
        assert_eq!(challenge, pkce_challenge(&verifier));
    }
}
