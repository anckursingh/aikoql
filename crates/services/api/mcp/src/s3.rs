//! MRFC-0080 R5a-2 / R6 — a minimal S3 client: SigV4-signed requests
//! over ureq, shared by the S3 lease and the S3 archive store.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(crate) struct Creds {
    pub(crate) akid: String,
    pub(crate) secret: String,
    pub(crate) token: Option<String>,
}

/// One bucket. `endpoint`: an S3-compatible store (path-style); `None`
/// is AWS (virtual-hosted).
pub(crate) struct S3 {
    base: String,
    host: String,
    /// "/bucket" for path-style, "" for virtual-hosted
    bucket_path: String,
    region: String,
    creds: Creds,
    agent: ureq::Agent,
}

impl S3 {
    /// `timeout` bounds every request.
    pub(crate) fn new(
        endpoint: Option<String>,
        bucket: String,
        region: String,
        creds: Creds,
        timeout: Duration,
    ) -> Self {
        let (base, bucket_path) = match endpoint {
            Some(e) => (e.trim_end_matches('/').to_string(), format!("/{bucket}")),
            None => (
                format!("https://{bucket}.s3.{region}.amazonaws.com"),
                String::new(),
            ),
        };
        let host = base.split("://").nth(1).unwrap_or(&base).to_string();
        Self {
            base,
            host,
            bucket_path,
            region,
            creds,
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
        }
    }

    /// The standard AWS credential and region variables;
    /// `AIKOQL_S3_ENDPOINT` selects an S3-compatible store.
    pub(crate) fn from_env(bucket: String, timeout: Duration) -> Result<Self, String> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let need = |k: &str| env(k).ok_or_else(|| format!("an s3:// location needs {k}"));
        let creds = Creds {
            akid: need("AWS_ACCESS_KEY_ID")?,
            secret: need("AWS_SECRET_ACCESS_KEY")?,
            token: env("AWS_SESSION_TOKEN"),
        };
        let region = env("AWS_REGION")
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| "us-east-1".into());
        Ok(Self::new(
            env("AIKOQL_S3_ENDPOINT"),
            bucket,
            region,
            creds,
            timeout,
        ))
    }

    /// One signed request on `key` ("" is the bucket root) with `query`
    /// pairs (unencoded) and an optional extra header.
    pub(crate) fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        body: &[u8],
        header: Option<(&str, &str)>,
    ) -> Result<ureq::Response, (Option<u16>, String)> {
        let (url, path, query) = self.target(key, query);
        let payload = format!("{:x}", Sha256::digest(body));
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let date = amz_date(secs);
        let mut headers = vec![
            ("host", self.host.as_str()),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", date.as_str()),
        ];
        if let Some(t) = &self.creds.token {
            headers.push(("x-amz-security-token", t.as_str()));
        }
        headers.sort();
        let auth = authorization(
            &self.creds,
            &self.region,
            method,
            &path,
            &query,
            &headers,
            &payload,
        );
        let mut req = self.agent.request(method, &url).set("authorization", &auth);
        for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
            req = req.set(k, v);
        }
        if let Some((k, v)) = header {
            req = req.set(k, v);
        }
        let sent = if method == "GET" {
            req.call()
        } else {
            req.send_bytes(body)
        };
        sent.map_err(describe)
    }
}

impl S3 {
    /// (URL, canonical path, canonical query) for `key` ("" is the bucket
    /// root) and unencoded `query` pairs. Virtual-hosted on AWS (the bucket
    /// is in the host), path-style on an endpoint (it leads the path).
    fn target(&self, key: &str, query: &[(&str, &str)]) -> (String, String, String) {
        let path = format!("{}/{}", self.bucket_path, uri_encode(key, true));
        let mut pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
            .collect();
        pairs.sort();
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let url = if query.is_empty() {
            format!("{}{path}", self.base)
        } else {
            format!("{}{path}?{query}", self.base)
        };
        (url, path, query)
    }
}

/// (HTTP status, if any; a description) — ureq's error is too large to
/// carry around.
pub(crate) fn describe(e: ureq::Error) -> (Option<u16>, String) {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body: String = body.chars().take(200).collect();
            (Some(code), format!("S3 {code}: {body}"))
        }
        ureq::Error::Transport(t) => (None, format!("S3 transport: {t}")),
    }
}

/// SigV4 URI encoding: unreserved bytes stay; `/` too in a path.
fn uri_encode(s: &str, path: bool) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b'/' if path => "/".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // justified: HMAC accepts keys of any length
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC key");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// The SigV4 `Authorization` header. `query` is the canonical query
/// string (sorted, encoded; "" for none); `headers` sorted, lowercase,
/// `x-amz-date` among them.
pub(crate) fn authorization(
    creds: &Creds,
    region: &str,
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    payload: &str,
) -> String {
    let date = headers
        .iter()
        .find(|(k, _)| *k == "x-amz-date")
        .map_or("", |(_, v)| *v);
    let day = date.get(..8).unwrap_or_default();
    let canonical: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed = headers
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(";");
    let request = format!("{method}\n{path}\n{query}\n{canonical}\n{signed}\n{payload}");
    let scope = format!("{day}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{:x}",
        Sha256::digest(request.as_bytes())
    );
    let mut key = hmac(format!("AWS4{}", creds.secret).as_bytes(), day.as_bytes());
    for part in [region, "s3", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let sig: String = hmac(&key, to_sign.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={sig}",
        creds.akid
    )
}

/// `YYYYMMDD'T'HHMMSS'Z'` for Unix seconds (civil-from-days, H. Hinnant).
pub(crate) fn amz_date(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The real-S3 certs' target: an S3-compatible endpoint
/// (AIKOQL_TEST_S3_ENDPOINT) or AWS itself (AIKOQL_TEST_S3_AWS=1 — no
/// endpoint, virtual-hosted URLs), with AIKOQL_TEST_S3_BUCKET, the AWS_*
/// credentials and AWS_REGION (default us-east-1). None — the certs skip —
/// when neither target is set.
#[cfg(test)]
pub(crate) struct RealS3 {
    pub(crate) endpoint: Option<String>,
    pub(crate) bucket: String,
    pub(crate) region: String,
    pub(crate) creds: Creds,
}

#[cfg(test)]
pub(crate) fn real_s3() -> Option<RealS3> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let endpoint = match (env("AIKOQL_TEST_S3_AWS"), env("AIKOQL_TEST_S3_ENDPOINT")) {
        (Some(_), _) => None,
        (None, Some(e)) => Some(e),
        (None, None) => return None,
    };
    Some(RealS3 {
        endpoint,
        bucket: env("AIKOQL_TEST_S3_BUCKET")?,
        region: env("AWS_REGION")
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| "us-east-1".into()),
        creds: Creds {
            akid: env("AWS_ACCESS_KEY_ID")?,
            secret: env("AWS_SECRET_ACCESS_KEY")?,
            token: env("AWS_SESSION_TOKEN"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Creds {
        Creds {
            akid: "AKID".into(),
            secret: "secret".into(),
            token: None,
        }
    }

    /// AWS (no endpoint): virtual-hosted — the bucket is in the host, the
    /// signed path is the key alone; a bucket-root list signs "/".
    #[test]
    fn aws_targets_are_virtual_hosted() {
        let s3 = S3::new(
            None,
            "my-bkt".into(),
            "eu-west-1".into(),
            creds(),
            Duration::from_secs(1),
        );
        assert_eq!(s3.host, "my-bkt.s3.eu-west-1.amazonaws.com");
        let (url, path, query) = s3.target("ha/lease json.json", &[]);
        assert_eq!(
            url,
            "https://my-bkt.s3.eu-west-1.amazonaws.com/ha/lease%20json.json"
        );
        assert_eq!(
            (path.as_str(), query.as_str()),
            ("/ha/lease%20json.json", "")
        );
        let (url, path, query) = s3.target("", &[("prefix", "a/b"), ("list-type", "2")]);
        assert_eq!(
            url,
            "https://my-bkt.s3.eu-west-1.amazonaws.com/?list-type=2&prefix=a%2Fb"
        );
        assert_eq!(
            (path.as_str(), query.as_str()),
            ("/", "list-type=2&prefix=a%2Fb")
        );
    }

    /// An S3-compatible endpoint: path-style — the bucket leads the path.
    #[test]
    fn endpoint_targets_are_path_style() {
        let s3 = S3::new(
            Some("http://127.0.0.1:9000/".into()),
            "bkt".into(),
            "us-east-1".into(),
            creds(),
            Duration::from_secs(1),
        );
        assert_eq!(s3.host, "127.0.0.1:9000");
        let (url, path, _) = s3.target("k", &[]);
        assert_eq!(
            (url.as_str(), path.as_str()),
            ("http://127.0.0.1:9000/bkt/k", "/bkt/k")
        );
        let (url, path, _) = s3.target("", &[("list-type", "2")]);
        assert_eq!(
            (url.as_str(), path.as_str()),
            ("http://127.0.0.1:9000/bkt/?list-type=2", "/bkt/")
        );
    }
}
