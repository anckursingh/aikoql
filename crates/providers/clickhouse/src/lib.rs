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
    base: String,
    database: String,
    user: Option<String>,
    password: Option<String>,
    agent: ureq::Agent,
    limit: Duration,
    version: String,
}

/// Did this error come from the deadline? Decided by the underlying `io::Error`
/// kind: a timeout kind (Unix: WouldBlock, Windows: TimedOut) is one at any speed,
/// while any other kind (refused, reset, …) is definitely not — Windows reports a
/// refused connection only after ~2s, which must not read as a timeout. Only
/// when no I/O error is visible does using up the whole budget mean a timeout.
fn is_timeout(err: &(dyn std::error::Error + 'static), elapsed: Duration, limit: Duration) -> bool {
    let mut saw_io = false;
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                return true;
            }
            saw_io = true;
        }
        cur = e.source();
    }
    !saw_io && elapsed >= limit
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `(base url without credentials, user, password)`. Never echoes the input.
fn split_url(url: &str) -> Result<(String, Option<String>, Option<String>), String> {
    let invalid = || "invalid ClickHouse URL".to_string();
    let (scheme, rest) = url.split_once("://").ok_or_else(invalid)?;
    if scheme != "http" && scheme != "https" {
        return Err(invalid());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (userinfo, host) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    if host.is_empty() {
        return Err(invalid());
    }
    let (user, pass) = match userinfo {
        None => (None, None),
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
            None => (Some(percent_decode(ui)), None),
        },
    };
    Ok((
        format!("{scheme}://{host}{}", path.trim_end_matches('/')),
        user,
        pass,
    ))
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A UInt64 that ClickHouse may send as a JSON number, a quoted string or null.
fn u64_field(v: &serde_json::Value, key: &str) -> u64 {
    match v.get(key) {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s))
            if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            s.parse().unwrap_or(u64::MAX)
        }
        _ => 0,
    }
}

/// Split a key expression list on top-level commas (`toDate(ts), id`).
fn split_key(expr: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for c in expr.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|k| !k.is_empty());
    out
}

fn unwrap_type(declared: &str) -> (String, bool, bool) {
    let (mut t, mut nullable, mut low) = (declared.trim(), false, false);
    loop {
        if let Some(inner) = t
            .strip_prefix("LowCardinality(")
            .and_then(|x| x.strip_suffix(')'))
        {
            low = true;
            t = inner.trim();
        } else if let Some(inner) = t
            .strip_prefix("Nullable(")
            .and_then(|x| x.strip_suffix(')'))
        {
            nullable = true;
            t = inner.trim();
        } else {
            return (t.to_string(), nullable, low);
        }
    }
}

impl ClickHouseConnector {
    /// Connect (one `SELECT version()`); `url` may carry `user:password@`.
    pub fn connect_with_timeout(
        url: &str,
        database: &str,
        timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        let (base, user, password) = split_url(url)?;
        let limit = Duration::from_millis(timeout_ms.unwrap_or(15_000).max(1));
        let agent = ureq::AgentBuilder::new()
            .timeout(limit)
            .redirects(0)
            .build();
        let mut c = ClickHouseConnector {
            base,
            database: database.to_string(),
            user,
            password,
            agent,
            limit,
            version: String::new(),
        };
        let rows = c.query("SELECT version()", &[])?;
        c.version = rows
            .first()
            .and_then(|r| r.get("version()"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .chars()
            .filter(|ch| ch.is_ascii_graphic())
            .take(64)
            .collect();
        Ok(c)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Run one read-only statement; `params` become `param_<name>` query
    /// parameters bound to `{name:Type}` placeholders in `sql`.
    fn query(&self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<serde_json::Value>, String> {
        let started = std::time::Instant::now();
        let mut req = self
            .agent
            .post(&format!("{}/", self.base))
            .query("readonly", "1")
            .query("default_format", "JSONEachRow")
            .query(
                "max_execution_time",
                &(self.limit.as_secs().max(1)).to_string(),
            );
        for (k, v) in params {
            req = req.query(&format!("param_{k}"), v);
        }
        if let Some(u) = &self.user {
            req = req.set("X-ClickHouse-User", u);
        }
        if let Some(p) = &self.password {
            req = req.set("X-ClickHouse-Key", p);
        }
        let resp = match req.send_string(sql) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(format!(
                    "clickhouse returned HTTP {code}: {}",
                    self.scrub(&body)
                ));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(if is_timeout(&t, started.elapsed(), self.limit) {
                    "clickhouse timed out".into()
                } else {
                    "clickhouse unreachable".into()
                });
            }
        };
        if resp.status() >= 300 {
            return Err(format!(
                "clickhouse sent an unexpected redirect (HTTP {})",
                resp.status()
            ));
        }
        use std::io::Read;
        let mut body = Vec::new();
        resp.into_reader()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| {
                if is_timeout(&e, started.elapsed(), self.limit) {
                    "clickhouse timed out".to_string()
                } else {
                    "clickhouse unreachable".to_string()
                }
            })?;
        if body.len() > MAX_BODY {
            return Err("clickhouse reply too large".into());
        }
        let text =
            String::from_utf8(body).map_err(|_| "clickhouse reply was not UTF-8".to_string())?;
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l).map_err(|_| "clickhouse reply was not JSON".to_string())
            })
            .collect()
    }

    /// A server message made safe to show: control characters out, the
    /// password (if any) redacted, length bounded.
    fn scrub(&self, msg: &str) -> String {
        let mut m: String = msg.chars().filter(|c| !c.is_control()).take(160).collect();
        if let Some(p) = self.password.as_deref().filter(|p| !p.is_empty()) {
            m = m.replace(p, "***");
        }
        m
    }

    /// Up to `max_tables + 1` tables (the extra row tells the caller the list
    /// was cut) with their columns and keys.
    pub fn introspect_all(&self, max_tables: usize) -> Result<Vec<TableSchema>, String> {
        let n = (max_tables + 1).to_string();
        let params = [("db", self.database.as_str()), ("n", n.as_str())];
        let tables = self.query(
            "SELECT name, engine, sorting_key, primary_key, partition_key, total_rows \
             FROM system.tables WHERE database = {db:String} AND NOT is_temporary \
             ORDER BY name LIMIT {n:UInt32}",
            &params,
        )?;
        let columns = self.query(
            "SELECT table, name, type, is_in_primary_key FROM system.columns \
             WHERE database = {db:String} AND table IN \
               (SELECT name FROM system.tables WHERE database = {db:String} AND NOT is_temporary \
                ORDER BY name LIMIT {n:UInt32}) \
             ORDER BY table, position",
            &params,
        )?;
        let mut out: Vec<TableSchema> = tables
            .iter()
            .take(max_tables + 1)
            .map(|t| TableSchema {
                name: str_field(t, "name"),
                engine: str_field(t, "engine"),
                columns: Vec::new(),
                sorting_key: split_key(&str_field(t, "sorting_key")),
                primary_key: split_key(&str_field(t, "primary_key")),
                partition_key: str_field(t, "partition_key"),
                row_count_estimate: u64_field(t, "total_rows"),
            })
            .collect();
        for c in &columns {
            let table = str_field(c, "table");
            let Some(t) = out.iter_mut().find(|t| t.name == table) else {
                continue;
            };
            let (ch_type, is_nullable, is_low_cardinality) = unwrap_type(&str_field(c, "type"));
            t.columns.push(ColumnInfo {
                name: str_field(c, "name"),
                ch_type,
                is_nullable,
                is_low_cardinality,
                in_primary_key: u64_field(c, "is_in_primary_key") != 0,
            });
        }
        Ok(out)
    }
}

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
        let e = c.introspect_all(10).expect_err("too large");
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
        let e = c.introspect_all(10).expect_err("timeout");
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
    fn a_refusal_is_never_a_timeout_however_long_it_took() {
        use std::io::{Error, ErrorKind};
        let (slow, quick, limit) = (
            Duration::from_secs(3),
            Duration::from_millis(5),
            Duration::from_secs(1),
        );
        // Windows reports a refused connection only after ~2s: still "unreachable"
        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::ConnectionReset,
            ErrorKind::AddrNotAvailable,
        ] {
            assert!(
                !is_timeout(&Wrapped(Error::from(kind)), slow, limit),
                "{kind:?} after {slow:?}"
            );
        }
        // the timeout kinds are timeouts at any speed (Unix: WouldBlock, Windows: TimedOut)
        for kind in [ErrorKind::TimedOut, ErrorKind::WouldBlock] {
            assert!(
                is_timeout(&Wrapped(Error::from(kind)), quick, limit),
                "{kind:?}"
            );
        }
        // no I/O error to read: only then does using up the whole budget mean a timeout
        let opaque = std::fmt::Error;
        assert!(is_timeout(&opaque, slow, limit));
        assert!(!is_timeout(&opaque, quick, limit));
    }
}
