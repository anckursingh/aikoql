//! ClickHouse schema discovery over the HTTP interface.
//!
//! Strictly read-only: every request carries `readonly=1`, only `system.tables`
//! and `system.columns` are queried, user text (database name) travels as an HTTP
//! query *parameter* (`param_db` + `{db:String}`), never spliced into SQL, and
//! credentials go in headers, never in a URL, a log line or an error.

use std::time::Duration;

/// Largest reply body accepted from the server.
pub const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    /// The declared type with `Nullable`/`LowCardinality` wrappers removed.
    pub ch_type: String,
    pub is_nullable: bool,
    pub is_low_cardinality: bool,
    pub in_primary_key: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableSchema {
    pub name: String,
    pub engine: String,
    pub columns: Vec<ColumnInfo>,
    pub sorting_key: Vec<String>,
    pub primary_key: Vec<String>,
    pub partition_key: String,
    pub row_count_estimate: u64,
}

pub struct ClickHouseConnector {
    #[allow(dead_code)]
    base: String,
    #[allow(dead_code)]
    database: String,
}

impl ClickHouseConnector {
    /// Connect (one `SELECT version()`); `url` may carry `user:password@`.
    pub fn connect_with_timeout(
        _url: &str,
        _database: &str,
        _timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        Err("todo".into())
    }

    /// Up to `max_tables` tables of the database with their columns and keys.
    pub fn introspect_all(&self, _max_tables: usize) -> Result<Vec<TableSchema>, String> {
        Ok(Vec::new())
    }

    pub fn version(&self) -> &str {
        ""
    }
}

#[allow(dead_code)]
const _: Duration = Duration::from_secs(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    struct Reply {
        status: u16,
        body: Vec<u8>,
        before: Duration,
        /// (bytes per chunk, pause) to drip the body slowly.
        drip: Option<(usize, Duration)>,
        location: Option<String>,
    }

    impl Reply {
        fn ok(body: &str) -> Self {
            Reply {
                status: 200,
                body: body.as_bytes().to_vec(),
                before: Duration::ZERO,
                drip: None,
                location: None,
            }
        }
        fn raw(status: u16, body: Vec<u8>) -> Self {
            Reply {
                status,
                body,
                before: Duration::ZERO,
                drip: None,
                location: None,
            }
        }
    }

    /// Routes by SQL text: `version()`, `system.tables`, `system.columns`.
    type Router = Box<dyn Fn(&str) -> Reply + Send>;

    fn serve(router: Router) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    let Ok(n) = s.read(&mut chunk) else {
                        break None;
                    };
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(p + 4);
                    }
                };
                let Some(end) = end else { continue };
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let want = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < end + want {
                    let Ok(n) = s.read(&mut chunk) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let request = String::from_utf8_lossy(&buf).to_string();
                let sql = String::from_utf8_lossy(&buf[end..]).to_string();
                log.lock().unwrap().push(request); // justified: Mutex poison is unrecoverable
                let r = router(&sql);
                std::thread::sleep(r.before);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    r.status,
                    r.body.len()
                );
                if let Some(l) = &r.location {
                    head.push_str(&format!("Location: {l}\r\n"));
                }
                head.push_str("\r\n");
                let _ = s.write_all(head.as_bytes());
                match r.drip {
                    None => {
                        let _ = s.write_all(&r.body);
                    }
                    Some((n, pause)) => {
                        for c in r.body.chunks(n) {
                            if s.write_all(c).is_err() {
                                break;
                            }
                            let _ = s.flush();
                            std::thread::sleep(pause);
                        }
                    }
                }
            }
        });
        (url, seen)
    }

    fn rows(lines: &[&str]) -> String {
        lines.join("\n") + "\n"
    }

    fn happy() -> Router {
        Box::new(|sql| {
            if sql.contains("version()") {
                Reply::ok(&rows(&[r#"{"version()":"24.8.4.13"}"#]))
            } else if sql.contains("system.tables") && !sql.contains("system.columns") {
                Reply::ok(&rows(&[
                    r#"{"name":"events","engine":"MergeTree","sorting_key":"toDate(ts), user_id","primary_key":"toDate(ts)","partition_key":"toYYYYMM(ts)","total_rows":"1234"}"#,
                    r#"{"name":"users","engine":"MergeTree","sorting_key":"id","primary_key":"id","partition_key":"","total_rows":null}"#,
                ]))
            } else if sql.contains("system.columns") {
                Reply::ok(&rows(&[
                    r#"{"table":"events","name":"ts","type":"DateTime","is_in_primary_key":1}"#,
                    r#"{"table":"events","name":"user_id","type":"UInt64","is_in_primary_key":0}"#,
                    r#"{"table":"events","name":"note","type":"Nullable(String)","is_in_primary_key":0}"#,
                    r#"{"table":"events","name":"kind","type":"LowCardinality(String)","is_in_primary_key":0}"#,
                    r#"{"table":"events","name":"tag","type":"LowCardinality(Nullable(String))","is_in_primary_key":0}"#,
                    r#"{"table":"users","name":"id","type":"UInt64","is_in_primary_key":1}"#,
                ]))
            } else {
                Reply::raw(400, b"unexpected query".to_vec())
            }
        })
    }

    fn connect(url: &str) -> Result<ClickHouseConnector, String> {
        ClickHouseConnector::connect_with_timeout(url, "analytics", Some(3_000))
    }

    #[test]
    fn introspect_all_parses_system_tables_and_columns() {
        let (url, _) = serve(happy());
        let c = connect(&url).unwrap();
        assert_eq!(c.version(), "24.8.4.13");
        let t = c.introspect_all(100).unwrap();
        assert_eq!(t.len(), 2);
        let ev = t.iter().find(|t| t.name == "events").unwrap();
        assert_eq!(ev.engine, "MergeTree");
        assert_eq!(
            ev.sorting_key,
            ["toDate(ts)", "user_id"],
            "top-level comma split keeps function calls whole"
        );
        assert_eq!(ev.primary_key, ["toDate(ts)"]);
        assert_eq!(ev.partition_key, "toYYYYMM(ts)");
        assert_eq!(ev.row_count_estimate, 1234);
        let col = |n: &str| ev.columns.iter().find(|c| c.name == n).unwrap();
        assert!(col("ts").in_primary_key && !col("user_id").in_primary_key);
        assert_eq!(
            (
                col("note").ch_type.as_str(),
                col("note").is_nullable,
                col("note").is_low_cardinality
            ),
            ("String", true, false)
        );
        assert_eq!(
            (
                col("kind").ch_type.as_str(),
                col("kind").is_nullable,
                col("kind").is_low_cardinality
            ),
            ("String", false, true)
        );
        assert_eq!(
            (col("tag").is_nullable, col("tag").is_low_cardinality),
            (true, true)
        );
        assert_eq!(
            t.iter()
                .find(|t| t.name == "users")
                .unwrap()
                .row_count_estimate,
            0,
            "NULL total_rows is 0"
        );
    }

    #[test]
    fn requests_are_readonly_parameterised_and_credentials_travel_in_headers() {
        let hostile = "x' OR 1=1; DROP TABLE t --";
        let (url, seen) = serve(happy());
        let with_creds = url.replace("http://", "http://alice:s3cr3t-pw@");
        let c =
            ClickHouseConnector::connect_with_timeout(&with_creds, hostile, Some(3_000)).unwrap();
        c.introspect_all(10).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 3);
        for req in seen.iter() {
            let line = req.lines().next().unwrap();
            assert!(line.contains("readonly=1"), "{line}");
            assert!(
                !line.contains("s3cr3t-pw") && !line.contains("alice"),
                "credentials leaked into the URL: {line}"
            );
            let lower = req.to_ascii_lowercase();
            assert!(
                lower.contains("x-clickhouse-user: alice")
                    && lower.contains("x-clickhouse-key: s3cr3t-pw"),
                "{req}"
            );
            let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
            assert!(
                !body.contains("DROP TABLE") && !body.contains("1=1"),
                "user text reached the SQL: {body}"
            );
            for verb in [
                "INSERT", "CREATE", "DROP", "ALTER", "TRUNCATE", "DELETE", "OPTIMIZE",
            ] {
                assert!(
                    !body.to_ascii_uppercase().contains(verb),
                    "write verb {verb} in: {body}"
                );
            }
        }
        assert!(
            seen.iter()
                .skip(1)
                .all(|r| r.lines().next().unwrap().contains("param_db=x%27")),
            "the database name must travel as an encoded parameter: {:?}",
            seen.iter()
                .map(|r| r.lines().next().unwrap().to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn errors_never_contain_the_password_or_the_host() {
        let (url, _) = serve(Box::new(|_| Reply::raw(500, b"Code: 999. boom".to_vec())));
        let with_creds = url.replace("http://", "http://alice:s3cr3t-pw@");
        let e = ClickHouseConnector::connect_with_timeout(&with_creds, "d", Some(2_000))
            .err()
            .unwrap();
        assert!(!e.contains("s3cr3t-pw") && !e.contains("127.0.0.1"), "{e}");
        assert!(e.contains("500"), "{e}");
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://alice:s3cr3t-pw@{}", l.local_addr().unwrap())
        };
        let e = ClickHouseConnector::connect_with_timeout(&dead, "d", Some(1_000))
            .err()
            .unwrap();
        assert!(!e.contains("s3cr3t-pw") && !e.contains("127.0.0.1"), "{e}");
        assert!(e.to_lowercase().contains("unreachable"), "{e}");
    }

    #[test]
    fn hostile_replies_are_short_errors_or_bounded_results_never_a_panic() {
        let routes: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("truncated json", br#"{"name":"events","eng"#.to_vec()),
            ("not json", b"<html>nope</html>".to_vec()),
            ("non-utf8", vec![0xff, 0xfe, 0x00, b'\n', 0x80]),
            ("deep nesting", format!("{}1{}\n", "[".repeat(50_000), "]".repeat(50_000)).into_bytes()),
            ("huge number", br#"{"name":"t","engine":"MergeTree","sorting_key":"","primary_key":"","partition_key":"","total_rows":"99999999999999999999999999"}"#.to_vec()),
            ("wrong types", br#"{"name":5,"engine":null,"sorting_key":[],"total_rows":{"a":1}}"#.to_vec()),
            ("nul in names", b"{\"name\":\"a\\u0000b\",\"engine\":\"E\",\"sorting_key\":\"\",\"primary_key\":\"\",\"partition_key\":\"\",\"total_rows\":\"1\"}\n".to_vec()),
        ];
        for (what, body) in routes {
            let b = body.clone();
            let (url, _) = serve(Box::new(move |sql| {
                if sql.contains("version()") {
                    Reply::ok(&rows(&[r#"{"version()":"24.8"}"#]))
                } else {
                    Reply::raw(200, b.clone())
                }
            }));
            let started = Instant::now();
            if let Ok(c) = connect(&url) {
                let _ = c.introspect_all(50); // Ok(bounded) or Err(short); must not panic
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{what}: took {:?}",
                started.elapsed()
            );
        }
    }

    #[test]
    fn oversized_bodies_are_rejected() {
        let big = vec![b'x'; MAX_BODY + 10];
        let (url, _) = serve(Box::new(move |sql| {
            if sql.contains("version()") {
                Reply::ok(&rows(&[r#"{"version()":"24.8"}"#]))
            } else {
                Reply::raw(200, big.clone())
            }
        }));
        let c = connect(&url).unwrap();
        let e = c.introspect_all(10).err().expect("too large");
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn thousands_of_tables_are_capped_by_max_tables() {
        let (url, seen) = serve(Box::new(|sql| {
            if sql.contains("version()") {
                Reply::ok(&rows(&[r#"{"version()":"24.8"}"#]))
            } else if sql.contains("system.columns") {
                Reply::ok("")
            } else {
                let lines: Vec<String> = (0..5_000)
                    .map(|i| format!(r#"{{"name":"t{i}","engine":"Log","sorting_key":"","primary_key":"","partition_key":"","total_rows":"1"}}"#))
                    .collect();
                Reply::ok(&(lines.join("\n") + "\n"))
            }
        }));
        let c = connect(&url).unwrap();
        let t = c.introspect_all(100).unwrap();
        assert!(t.len() <= 101, "got {}", t.len());
        let all = seen.lock().unwrap().join("\n");
        assert!(
            all.contains("LIMIT"),
            "the cap must also be sent to the server"
        );
    }

    #[test]
    fn a_slow_drip_body_times_out_instead_of_hanging() {
        let (url, _) = serve(Box::new(|sql| {
            if sql.contains("version()") {
                Reply::ok(&rows(&[r#"{"version()":"24.8"}"#]))
            } else {
                Reply {
                    status: 200,
                    body: vec![b'x'; 200],
                    before: Duration::ZERO,
                    drip: Some((1, Duration::from_millis(100))),
                    location: None,
                }
            }
        }));
        let c = ClickHouseConnector::connect_with_timeout(&url, "d", Some(1_000)).unwrap();
        let t = Instant::now();
        let e = c.introspect_all(10).err().expect("timeout");
        assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
        assert!(e.contains("timed out"), "{e}");
    }

    #[test]
    fn redirects_auth_failures_and_server_errors_are_reported_not_followed() {
        for (status, want) in [
            (401u16, "401"),
            (403, "403"),
            (500, "500"),
            (302, "redirect"),
        ] {
            let (url, seen) = serve(Box::new(move |_| {
                let mut r = Reply::raw(status, b"nope".to_vec());
                if status == 302 {
                    r.location = Some("http://127.0.0.1:1/elsewhere".into());
                }
                r
            }));
            let e = connect(&url).err().unwrap_or_default();
            assert!(e.contains(want), "{status}: {e}");
            assert_eq!(
                seen.lock().unwrap().len(),
                1,
                "{status}: must not retry or follow"
            );
        }
    }
}
