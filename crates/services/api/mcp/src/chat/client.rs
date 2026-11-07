//! Model endpoint configuration (`[chat]`) and, later, the OpenAI-style client.

use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::time::Duration;

/// Cap on the model's reply body.
const MAX_BODY: usize = 256 * 1024;

/// Set once at startup when `[chat]` is present; absent = chat disabled.
pub(crate) static CHAT_CONFIG: std::sync::OnceLock<ChatConfig> = std::sync::OnceLock::new();

fn default_timeout() -> u64 {
    60
}

/// `[chat]` table: an OpenAI-compatible chat-completions endpoint.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatConfig {
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl ChatConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
            return Err("url must start with http:// or https://".into());
        }
        if self.model.trim().is_empty() {
            return Err("model must not be empty".into());
        }
        if self.timeout_secs == 0 {
            return Err("timeout_secs must be at least 1".into());
        }
        Ok(())
    }
}

/// Short-timeout TCP connect to the endpoint's host:port.
pub(crate) fn reachable(url: &str, timeout: Duration) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let hostport = rest.split('/').next().unwrap_or("");
    if hostport.is_empty() {
        return false;
    }
    let addr = if hostport.contains(':') && !hostport.ends_with(']') {
        hostport.to_string()
    } else {
        format!("{hostport}:{}", if scheme == "https" { 443 } else { 80 })
    };
    addr.to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| TcpStream::connect_timeout(&a, timeout).is_ok())
}

/// Did this transport error come from the deadline? Decided by the underlying
/// `io::Error` kind (Unix reports a socket timeout as WouldBlock, Windows as
/// TimedOut with text that never says "timed out"), and otherwise by the call
/// having used up its whole budget.
fn is_timeout(
    _err: &(dyn std::error::Error + 'static),
    _elapsed: Duration,
    _limit: Duration,
) -> bool {
    false
}

/// One prompt in, one completion out.
pub(crate) trait Generate {
    fn generate(&self, prompt: &str) -> Result<String, String>;
}

/// OpenAI-style chat-completions client.
pub(crate) struct HttpModel {
    cfg: ChatConfig,
}

impl HttpModel {
    pub(crate) fn new(cfg: ChatConfig) -> Self {
        Self { cfg }
    }
}

impl Generate for HttpModel {
    fn generate(&self, prompt: &str) -> Result<String, String> {
        let base = self.cfg.url.trim_end_matches('/');
        let url = if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/v1/chat/completions")
        };
        let mut req = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(self.cfg.timeout_secs))
            .build()
            .post(&url);
        if let Some(name) = &self.cfg.api_key_env {
            let key = std::env::var(name)
                .map_err(|_| format!("model api key env var {name} is not set"))?;
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let resp = req
            .send_json(json!({
                "model": self.cfg.model,
                "messages": [{"role": "user", "content": prompt}],
                "temperature": 0,
            }))
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => format!("model returned HTTP {code}"),
                ureq::Error::Transport(t) if t.to_string().contains("timed out") => {
                    "model timed out".into()
                }
                ureq::Error::Transport(_) => "model unreachable".into(),
            })?;
        let mut body = Vec::new();
        resp.into_reader()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut {
                    "model timed out".to_string()
                } else {
                    "model unreachable".to_string()
                }
            })?;
        if body.len() > MAX_BODY {
            return Err("model reply too large".into());
        }
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["choices"][0]["message"]["content"]
                    .as_str()
                    .map(String::from)
            })
            .ok_or_else(|| "model reply was not JSON".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::stub::{spawn_stub_server, StubResponse};
    use std::time::{Duration, Instant};

    fn cfg(url: &str) -> ChatConfig {
        ChatConfig {
            url: url.into(),
            model: "m".into(),
            api_key_env: None,
            timeout_secs: 5,
        }
    }

    #[derive(Debug)]
    struct Wrapped(std::io::Error);
    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrapped")
        }
    }
    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn is_timeout_reads_the_io_kind_not_the_message() {
        use std::io::{Error, ErrorKind};
        let (quick, limit) = (Duration::from_millis(5), Duration::from_secs(1));
        // the Windows wording carries no "timed out" at all
        let win = Error::new(
            ErrorKind::TimedOut,
            "os error 10060: the connected party did not properly respond",
        );
        assert!(is_timeout(&win, quick, limit));
        assert!(is_timeout(
            &Error::from(ErrorKind::WouldBlock),
            quick,
            limit
        ));
        assert!(
            is_timeout(&Wrapped(Error::from(ErrorKind::TimedOut)), quick, limit),
            "found down the source chain"
        );
        // a refusal that returned at once is not a timeout
        assert!(!is_timeout(
            &Error::from(ErrorKind::ConnectionRefused),
            quick,
            limit
        ));
        // an unclassified failure after the whole budget is
        assert!(is_timeout(
            &Error::other("odd"),
            Duration::from_secs(1),
            limit
        ));
    }

    #[test]
    fn reachable_probes_the_url_host_and_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", l.local_addr().unwrap());
        assert!(reachable(&url, Duration::from_secs(2)));
        drop(l);
        assert!(!reachable(&url, Duration::from_secs(2)));
        assert!(!reachable("not a url", Duration::from_secs(1)));
    }

    #[test]
    fn returns_the_message_content() {
        let (url, seen) = spawn_stub_server(vec![StubResponse::ok("hello")]);
        let out = HttpModel::new(cfg(&url)).generate("ping").unwrap();
        assert_eq!(out, "hello");
        let req = seen.lock().unwrap()[0].clone();
        assert!(req.contains("\"model\":\"m\""), "{req}");
        assert!(req.contains("\"content\":\"ping\""), "{req}");
        assert!(req.contains("\"temperature\":0"), "{req}");
    }

    #[test]
    fn sends_bearer_key_from_env_var_only_when_configured() {
        let (url, seen) = spawn_stub_server(vec![StubResponse::ok("x")]);
        HttpModel::new(cfg(&url)).generate("p").unwrap();
        // PATH is always set; the test only reads the environment
        let mut c = cfg(&url);
        c.api_key_env = Some("PATH".into());
        HttpModel::new(c).generate("p").unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            !seen[0].to_lowercase().contains("authorization"),
            "{}",
            seen[0]
        );
        let want = format!("Bearer {}", std::env::var("PATH").unwrap());
        assert!(seen[1].contains(&want), "{}", seen[1]);
    }

    #[test]
    fn unset_key_env_var_is_an_error_not_an_unauthenticated_call() {
        let (url, seen) = spawn_stub_server(vec![StubResponse::ok("x")]);
        let mut c = cfg(&url);
        c.api_key_env = Some("AIKOQL_CHAT_TEST_KEY_NEVER_SET".into());
        let e = HttpModel::new(c).generate("p").unwrap_err();
        assert!(e.contains("not set"), "{e}");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn unreachable_port_is_model_unreachable() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        assert_eq!(
            HttpModel::new(cfg(&url)).generate("p").unwrap_err(),
            "model unreachable"
        );
    }

    #[test]
    fn slow_server_times_out() {
        let mut r = StubResponse::ok("late");
        r.delay = Duration::from_secs(4);
        let (url, _) = spawn_stub_server(vec![r]);
        let mut c = cfg(&url);
        c.timeout_secs = 1;
        let t = Instant::now();
        assert_eq!(
            HttpModel::new(c).generate("p").unwrap_err(),
            "model timed out"
        );
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn http_500_is_reported() {
        let (url, _) = spawn_stub_server(vec![StubResponse::raw(500, "boom")]);
        assert_eq!(
            HttpModel::new(cfg(&url)).generate("p").unwrap_err(),
            "model returned HTTP 500"
        );
    }

    #[test]
    fn non_json_body_is_reported() {
        let (url, _) = spawn_stub_server(vec![StubResponse::raw(200, "<html>")]);
        assert_eq!(
            HttpModel::new(cfg(&url)).generate("p").unwrap_err(),
            "model reply was not JSON"
        );
    }

    #[test]
    fn json_without_choices_is_reported() {
        let (url, _) = spawn_stub_server(vec![StubResponse::raw(200, "{\"a\":1}")]);
        assert_eq!(
            HttpModel::new(cfg(&url)).generate("p").unwrap_err(),
            "model reply was not JSON"
        );
    }

    #[test]
    fn oversized_body_is_rejected() {
        let big = "x".repeat(300 * 1024);
        let (url, _) = spawn_stub_server(vec![StubResponse::ok(&big)]);
        assert_eq!(
            HttpModel::new(cfg(&url)).generate("p").unwrap_err(),
            "model reply too large"
        );
    }

    #[test]
    fn url_without_path_gets_the_default_path() {
        let (url, seen) = spawn_stub_server(vec![StubResponse::ok("x")]);
        HttpModel::new(cfg(&format!("{url}/")))
            .generate("p")
            .unwrap();
        HttpModel::new(cfg(&format!("{url}/custom/chat/completions")))
            .generate("p")
            .unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            seen[0].starts_with("POST /v1/chat/completions "),
            "{}",
            seen[0]
        );
        assert!(
            seen[1].starts_with("POST /custom/chat/completions "),
            "{}",
            seen[1]
        );
    }
}
