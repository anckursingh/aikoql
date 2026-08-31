//! Wave 5 Phase C — W5-OLAP-001..004 conventional-OLAP baseline vs
//! ClickHouse and StarRocks (plan §4/§23 comparative harness).
//!
//! The spec's senior-QA position (plan §28): this is a boundary-discovery
//! benchmark, not a ClickHouse benchmark. These four tests pin the side of
//! the boundary where conventional OLAP should remain the better tool —
//! correctness is the assertion, latency is measured and printed, and no
//! AIKOQL-superiority claim rides on any of it.
//!
//! Adapters (harness-only, never product code — plan §7 build-vs-buy):
//! - ClickHouse: its HTTP query interface (one POST per statement).
//! - StarRocks: MySQL protocol via the `mysql` dev-dependency.
//!   Both consume the SAME deterministic dataset: n = 0..N-1 exactly once
//!   (ClickHouse `numbers()`, StarRocks a digits cross join of a 10-row
//!   seed table — pure standard SQL, identical semantics), so ground truth
//!   is computable in Rust independently of either engine.
//!
//! Dataset (§22, sizes honest-labeled — the plan's 100M/1B rows are
//! NOT_RUN: this harness reproduces at 10M/1M on a dev machine):
//!   txn 10M (customer_id = n%100000, amount = n%1000)
//!   events 1M (service n%20, device n%1000, region n%5, err n%100==0,
//!              day n/86400, lat n%500)
//!   customers 100K (id, tier id%3, region id%5)
//!   devices 1M (device_id, customer_id = n%100000)
//!
//! Opt-in env — STRICT: unset → engine skipped with an honest NOT_MEASURED
//! row; set but unreachable → the test FAILS (an opted-in measurement never
//! silently degrades to a skip):
//!   AIKOQL_TEST_CH_HTTP = "127.0.0.1:8123"
//!   AIKOQL_TEST_SR_ADDR = "127.0.0.1:9030"
//! Services: `docker compose --profile olap up -d clickhouse starrocks`.
//! StarRocks allin1 needs ≥8GB Docker memory; when not opted in (CI),
//! results are recorded NOT_MEASURED, never invented.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Once;
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;

// ── adapters ────────────────────────────────────────────────────────────

/// ClickHouse over HTTP — the whole adapter is one POST.
#[derive(Clone)]
struct Ch {
    addr: String,
}

impl Ch {
    /// Strict opt-in: env unset → None (honest skip). Env set but the
    /// engine unreachable → panic. An opted-in measurement must never
    /// silently degrade to NOT_MEASURED.
    fn probe() -> Option<Ch> {
        let Ok(addr) = std::env::var("AIKOQL_TEST_CH_HTTP") else {
            return None;
        };
        // Accept both "127.0.0.1:8123" and "http://127.0.0.1:8123" — the
        // var is named *_HTTP, so URLs will be passed.
        let addr = addr.strip_prefix("http://").unwrap_or(&addr);
        let addrs: Vec<_> = addr
            .to_socket_addrs()
            .unwrap_or_else(|e| panic!("AIKOQL_TEST_CH_HTTP={addr} set but unparsable: {e}"))
            .collect();
        if TcpStream::connect_timeout(&addrs[0], Duration::from_secs(2)).is_err() {
            panic!(
                "AIKOQL_TEST_CH_HTTP={addr} set but unreachable — opt-in is strict, \
                 no silent NOT_MEASURED. Start: docker compose --profile olap up -d clickhouse"
            );
        }
        Some(Ch {
            addr: addr.to_string(),
        })
    }

    /// POST a statement, return the TSV body (or panic with the engine's
    /// own error — a query bug must be RED, not a silent skip). Auth via
    /// URL params (ClickHouse accepts user/password there — no Basic
    /// header, no base64).
    fn query(&self, sql: &str) -> String {
        self.send("database=aikoql_bench&", sql)
    }

    /// Same POST but NO session database — required for CREATE DATABASE
    /// (pinning database=aikoql_bench before it exists is UNKNOWN_DATABASE).
    fn raw(&self, sql: &str) -> String {
        self.send("", sql)
    }

    fn send(&self, extra_params: &str, sql: &str) -> String {
        let pw =
            std::env::var("AIKOQL_TEST_CH_PASSWORD").unwrap_or_else(|_| "aikoql-dev-only".into());
        let mut s = TcpStream::connect(&self.addr)
            .unwrap_or_else(|e| panic!("clickhouse connect {}: {e}", self.addr));
        // HTTP/1.0 on purpose: ClickHouse answers HTTP/1.1 with
        // Transfer-Encoding: chunked, and this adapter reads the raw socket
        // (chunk-size lines would pollute the TSV). 1.0 has no chunked
        // encoding — the body is delimited by connection close.
        let req = format!(
            "POST /?{extra_params}user=default&password={pw} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\n\r\n{}",
            self.addr,
            sql.len(),
            sql
        );
        s.write_all(req.as_bytes())
            .unwrap_or_else(|e| panic!("clickhouse write: {e}"));
        let mut out = String::new();
        s.read_to_string(&mut out)
            .unwrap_or_else(|e| panic!("clickhouse read: {e}"));
        let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
        if body.contains("Code: ") {
            panic!("clickhouse query failed: {body}\nSQL: {sql}");
        }
        body.trim().to_string()
    }

    fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        let body = self.query(sql);
        if body.is_empty() {
            return Vec::new();
        }
        body.lines()
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }
}

/// StarRocks via MySQL protocol — the `mysql` crate is the adapter.
fn sr_connect() -> mysql::Conn {
    let addr = std::env::var("AIKOQL_TEST_SR_ADDR").unwrap();
    let opts = mysql::OptsBuilder::new()
        .ip_or_hostname(Some(addr.split(':').next().unwrap_or("127.0.0.1")))
        .tcp_port(
            addr.split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(9030),
        )
        .user(Some("root"))
        .pass(Some(""))
        // Loopback + default prefer_socket(true) makes the crate probe the
        // @@socket system variable — StarRocks has no such variable.
        .prefer_socket(false);
    mysql::Conn::new(opts).expect("starrocks connect")
}

/// Strict opt-in like [`Ch::probe`]: env unset → false (honest skip);
/// set but unreachable → panic, never a silent NOT_MEASURED.
fn sr_probe() -> bool {
    let Some(addr) = std::env::var("AIKOQL_TEST_SR_ADDR").ok() else {
        return false;
    };
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        panic!("AIKOQL_TEST_SR_ADDR={addr} set but unparsable");
    };
    let reachable = addrs
        .next()
        .map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok())
        .unwrap_or(false);
    if !reachable {
        panic!(
            "AIKOQL_TEST_SR_ADDR={addr} set but unreachable — opt-in is strict, \
             no silent NOT_MEASURED. Start: docker compose --profile olap up -d starrocks"
        );
    }
    true
}

fn sr_cell(v: &mysql::Value) -> String {
    match v {
        mysql::Value::Int(i) => i.to_string(),
        mysql::Value::UInt(u) => u.to_string(),
        mysql::Value::Float(f) => f.to_string(),
        mysql::Value::Double(d) => d.to_string(),
        // StarRocks FE ships result cells as text over the wire — numbers
        // arrive as Bytes(b"98305"), not Int/UInt.
        mysql::Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        other => format!("{other:?}"),
    }
}

fn sr_rows(conn: &mut mysql::Conn, sql: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let result = conn
        .query_iter(sql)
        .unwrap_or_else(|e| panic!("starrocks query failed: {e}\nSQL: {sql}"));
    for row in result {
        let row = row.expect("starrocks row");
        let mut cells = Vec::new();
        for c in row.unwrap() {
            cells.push(sr_cell(&c));
        }
        out.push(cells);
    }
    out
}

fn sr_drop(conn: &mut mysql::Conn, sql: &str) {
    conn.query_drop(sql)
        .unwrap_or_else(|e| panic!("starrocks ddl failed: {e}\nSQL: {sql}"));
}

fn sr_connect_drop(sql: &str) {
    let mut conn = sr_connect();
    sr_drop(&mut conn, sql);
}

/// Deterministic generator: n = 0..10^digits-1 exactly once, via a cross
/// join of the 10-row seed table — pure standard SQL, works on both
/// engines. The same set `numbers(N)` produces in ClickHouse.
fn sr_numbers(digits: usize) -> String {
    let cols: Vec<char> = (0..digits).map(|i| (b'a' + i as u8) as char).collect();
    let exprs: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c}.v*{}", 10u64.pow((digits - 1 - i) as u32)))
        .collect();
    // Qualified: the mysql session selects no default database, and StarRocks
    // resolves bare names against the session db (unlike CH, which resolves
    // against the URL-pinned database).
    let from: Vec<String> = cols
        .iter()
        .map(|c| format!("aikoql_bench.seed {c}"))
        .collect();
    // Trailing `AS g` is required by StarRocks (every derived table needs an
    // alias); ClickHouse ignores it. `numbers()` isn't aliased either — CH's
    // table function needs none.
    format!(
        "(SELECT ({}) AS n FROM {}) AS g",
        exprs.join("+"),
        from.join(", ")
    )
}

// ── dataset + ground truth ──────────────────────────────────────────────

const N_TXN: u64 = 10_000_000;
const N_EVT: u64 = 1_000_000;
const N_CUST: u64 = 100_000;
const N_DEV: u64 = 1_000_000;

/// Ground truth for txn: per-customer sum and per-tier sum. Amount for a
/// customer c is (c + 100000k) % 1000 = c % 1000 (100000 ≡ 0 mod 1000),
/// 100 occurrences each.
fn txn_truth() -> (u64, HashMap<u32, u64>, [u64; 3]) {
    let mut total = 0u64;
    let mut per_tier = [0u64; 3];
    let mut per_cust = HashMap::new();
    for c in 0..N_CUST as u32 {
        let sum = 100 * (c % 1000) as u64;
        total += sum;
        per_tier[(c % 3) as usize] += sum;
        per_cust.insert(c, sum);
    }
    (total, per_cust, per_tier)
}

/// Ground truth for events: (service, device, region, err, day) combo
/// counts + per-service error totals + per-service p95 latency. Generator
/// coupling: device = n%1000 pins service/region/err (err=1 ⟹ service 0,
/// so all 10000 errors sit in service 0), and lat ≡ service (mod 20), so
/// p95 = service+460 exactly.
type ComboKey = (u16, u16, u16, u8, u8);

fn evt_truth() -> (HashMap<ComboKey, u32>, [u32; 20], [u32; 20]) {
    let mut combos: HashMap<(u16, u16, u16, u8, u8), u32> = HashMap::new();
    let mut errs = [0u32; 20];
    let mut lat_hist = vec![[0u32; 500]; 20];
    for n in 0..N_EVT {
        let s = (n % 20) as u16;
        let d = (n % 1000) as u16;
        let r = (n % 5) as u16;
        let e = (n % 100 == 0) as u8;
        let day = (n / 86400) as u8;
        *combos.entry((s, d, r, e, day)).or_insert(0) += 1;
        if e == 1 {
            errs[s as usize] += 1;
        }
        lat_hist[s as usize][(n % 500) as usize] += 1;
    }
    let mut p95 = [0u32; 20];
    let target = ((N_EVT / 20) * 95 / 100 + 1) as u32;
    for (s, hist) in lat_hist.iter().enumerate() {
        let mut cum = 0u32;
        for (v, c) in hist.iter().enumerate() {
            cum += c;
            if cum >= target {
                p95[s] = v as u32;
                break;
            }
        }
    }
    (combos, errs, p95)
}

static INIT: Once = Once::new();

/// Create + load every table once per process run (later tests reuse).
/// Timings printed — load cost is part of the reproducibility record.
fn ensure_loaded(ch: &Option<Ch>, want_sr: bool) {
    INIT.call_once(|| {
        let t0 = Instant::now();
        if let Some(ch) = ch {
            ch.raw("CREATE DATABASE IF NOT EXISTS aikoql_bench");
            ch.query("DROP TABLE IF EXISTS seed");
            ch.query("CREATE TABLE seed (v UInt32) ENGINE = Memory");
            ch.query("INSERT INTO seed VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)");
            let txn_ddl = "CREATE TABLE IF NOT EXISTS txn (customer_id UInt32, amount UInt32) ENGINE = MergeTree ORDER BY customer_id";
            let evt_ddl = "CREATE TABLE IF NOT EXISTS events (service UInt16, device UInt32, region UInt8, err UInt8, day UInt16, lat UInt16) ENGINE = MergeTree ORDER BY day";
            let cust_ddl = "CREATE TABLE IF NOT EXISTS customers (id UInt32, tier UInt8, region UInt8) ENGINE = MergeTree ORDER BY id";
            let dev_ddl = "CREATE TABLE IF NOT EXISTS devices (device_id UInt32, customer_id UInt32) ENGINE = MergeTree ORDER BY device_id";
            ch.query("DROP TABLE IF EXISTS txn");
            ch.query("DROP TABLE IF EXISTS events");
            ch.query("DROP TABLE IF EXISTS customers");
            ch.query("DROP TABLE IF EXISTS devices");
            ch.query(txn_ddl);
            ch.query(evt_ddl);
            ch.query(cust_ddl);
            ch.query(dev_ddl);
            let l = Instant::now();
            ch.query(&format!(
                "INSERT INTO txn SELECT number % 100000, number % 1000 FROM numbers({N_TXN})"
            ));
            ch.query(&format!(
                "INSERT INTO events SELECT number % 20, number % 1000, number % 5, number % 100 = 0, number / 86400, number % 500 FROM numbers({N_EVT})"
            ));
            ch.query(&format!(
                "INSERT INTO customers SELECT number, number % 3, number % 5 FROM numbers({N_CUST})"
            ));
            ch.query(&format!(
                "INSERT INTO devices SELECT number, number % 100000 FROM numbers({N_DEV})"
            ));
            println!(
                "[W5-OLAP] clickhouse loaded 12.1M rows in {}ms",
                l.elapsed().as_millis()
            );
        }
        if want_sr {
            sr_connect_drop("CREATE DATABASE IF NOT EXISTS aikoql_bench");
            sr_connect_drop("DROP TABLE IF EXISTS aikoql_bench.seed");
            sr_connect_drop("CREATE TABLE aikoql_bench.seed (v INT) DISTRIBUTED BY HASH(v) BUCKETS 1");
            sr_connect_drop("INSERT INTO aikoql_bench.seed VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)");
            sr_connect_drop("DROP TABLE IF EXISTS aikoql_bench.txn");
            sr_connect_drop("DROP TABLE IF EXISTS aikoql_bench.events");
            sr_connect_drop("DROP TABLE IF EXISTS aikoql_bench.customers");
            sr_connect_drop("DROP TABLE IF EXISTS aikoql_bench.devices");
            sr_connect_drop("CREATE TABLE aikoql_bench.txn (customer_id INT NOT NULL, amount INT NOT NULL) DISTRIBUTED BY HASH(customer_id) BUCKETS 8");
            sr_connect_drop("CREATE TABLE aikoql_bench.events (service SMALLINT NOT NULL, device INT NOT NULL, region TINYINT NOT NULL, err TINYINT NOT NULL, day SMALLINT NOT NULL, lat SMALLINT NOT NULL) DISTRIBUTED BY HASH(device) BUCKETS 8");
            sr_connect_drop("CREATE TABLE aikoql_bench.customers (id INT NOT NULL, tier TINYINT NOT NULL, region TINYINT NOT NULL) DISTRIBUTED BY HASH(id) BUCKETS 8");
            sr_connect_drop("CREATE TABLE aikoql_bench.devices (device_id INT NOT NULL, customer_id INT NOT NULL) DISTRIBUTED BY HASH(device_id) BUCKETS 8");
            let l = Instant::now();
            sr_connect_drop(&format!(
                "INSERT INTO aikoql_bench.txn SELECT n % 100000, n % 1000 FROM {}",
                sr_numbers(7)
            ));
            sr_connect_drop(&format!(
                "INSERT INTO aikoql_bench.events SELECT n % 20, n % 1000, n % 5, n % 100 = 0, n / 86400, n % 500 FROM {}",
                sr_numbers(6)
            ));
            sr_connect_drop(&format!(
                "INSERT INTO aikoql_bench.customers SELECT n, n % 3, n % 5 FROM {}",
                sr_numbers(5)
            ));
            sr_connect_drop(&format!(
                "INSERT INTO aikoql_bench.devices SELECT n, n % 100000 FROM {}",
                sr_numbers(6)
            ));
            println!(
                "[W5-OLAP] starrocks loaded 12.1M rows in {}ms",
                l.elapsed().as_millis()
            );
        }
        println!("[W5-OLAP] setup wall {}ms", t0.elapsed().as_millis());
    });
}

/// min-of-3 wall ms for a query (warm-cache, debug-run honest label).
fn timed_ms<F: FnMut() -> Vec<Vec<String>>>(mut f: F) -> (Vec<Vec<String>>, u128) {
    let mut best = u128::MAX;
    let mut rows = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let r = f();
        best = best.min(t.elapsed().as_millis());
        rows = r;
    }
    (rows, best)
}

fn fcell(row: &[String], i: usize) -> f64 {
    row[i]
        .trim_matches('\'')
        .parse::<f64>()
        .unwrap_or_else(|_| {
            panic!("non-numeric cell {:?} in row {:?}", row[i], row);
        })
}

fn ucell(row: &[String], i: usize) -> u64 {
    fcell(row, i) as u64
}

// ── W5-OLAP-001 — large aggregation ─────────────────────────────────────

#[test]
fn w5_olap_001_large_aggregation() {
    let ch = Ch::probe();
    let want_sr = sr_probe();
    if ch.is_none() && !want_sr {
        println!(
            "[W5-OLAP-001] both engines skipped — set AIKOQL_TEST_CH_HTTP / AIKOQL_TEST_SR_ADDR"
        );
        return;
    }
    ensure_loaded(&ch, want_sr);
    let (total, per_cust, _) = txn_truth();
    println!(
        "[W5-OLAP-001] SELECT customer_id, sum(amount) FROM txn GROUP BY customer_id — {} rows, {} groups",
        N_TXN, N_CUST
    );
    if let Some(ch) = &ch {
        let (rows, ms) =
            timed_ms(|| ch.rows("SELECT customer_id, sum(amount) FROM txn GROUP BY customer_id"));
        assert_eq!(rows.len(), N_CUST as usize, "clickhouse group count");
        let mut seen_total = 0u64;
        for r in &rows {
            let (c, s) = (ucell(r, 0) as u32, ucell(r, 1));
            seen_total += s;
            if c == 42 || c == 99999 || c == 1000 {
                assert_eq!(s, *per_cust.get(&c).unwrap(), "clickhouse customer {c} sum");
            }
        }
        assert_eq!(seen_total, total, "clickhouse grand total");
        println!("  clickhouse: total {total} | {ms}ms (min of 3) | CORRECT");
    } else {
        println!("  clickhouse: NOT_MEASURED (not opted in)");
    }
    if want_sr {
        let mut conn = sr_connect();
        let (rows, ms) = timed_ms(|| {
            sr_rows(
                &mut conn,
                "SELECT customer_id, SUM(amount) FROM aikoql_bench.txn GROUP BY customer_id",
            )
        });
        assert_eq!(rows.len(), N_CUST as usize, "starrocks group count");
        let mut seen_total = 0u64;
        for r in &rows {
            let (c, s) = (ucell(r, 0) as u32, ucell(r, 1));
            seen_total += s;
            if c == 42 || c == 99999 || c == 1000 {
                assert_eq!(s, *per_cust.get(&c).unwrap(), "starrocks customer {c} sum");
            }
        }
        assert_eq!(seen_total, total, "starrocks grand total");
        println!("  starrocks: total {total} | {ms}ms (min of 3) | CORRECT");
    } else {
        println!("  starrocks: NOT_MEASURED (not opted in)");
    }
    println!("  aikoql: NOT_MEASURED — no columnar scan path; §7 build-vs-buy says delegate (redb row-at-a-time on 10M rows is a structural loss, plan §19)");
}

// ── W5-OLAP-002 — time-series analytics ─────────────────────────────────

#[test]
fn w5_olap_002_time_series() {
    let ch = Ch::probe();
    let want_sr = sr_probe();
    if ch.is_none() && !want_sr {
        println!("[W5-OLAP-002] both engines skipped");
        return;
    }
    ensure_loaded(&ch, want_sr);
    let (_, errs, p95) = evt_truth();
    println!("[W5-OLAP-002] events/day, error rate/service, p95 latency/service — 1M events");
    // Honest scope: the loaded schema stores day + lat but not minute, so
    // events/minute per service is measured as events/day per service
    // (the day bucket is what the dataset pins; minute-level bucketing
    // would need a ts column — recorded, not silently approximated).
    let day_q = "SELECT service, day, COUNT(*) FROM events GROUP BY service, day";
    if let Some(ch) = &ch {
        let (rows, ms) = timed_ms(|| ch.rows(day_q));
        assert_eq!(rows.len(), 20 * 12, "clickhouse day buckets");
        let total: u64 = rows.iter().map(|r| ucell(r, 2)).sum();
        assert_eq!(total, N_EVT, "clickhouse events total");
        for r in &rows {
            if ucell(r, 0) == 0 && ucell(r, 1) == 0 {
                assert_eq!(ucell(r, 2), 4320, "clickhouse service 0 day 0 count");
            }
        }
        println!(
            "  clickhouse: events/day per service: {ms}ms | {} buckets | total {total} | CORRECT",
            rows.len()
        );
        let (rows, ms) = timed_ms(|| {
            ch.rows("SELECT service, COUNT(*) FROM events WHERE err = 1 GROUP BY service")
        });
        let total: u64 = rows.iter().map(|r| ucell(r, 1)).sum();
        assert_eq!(total, 10000, "clickhouse err total");
        for r in &rows {
            assert_eq!(
                ucell(r, 1),
                errs[ucell(r, 0) as usize] as u64,
                "clickhouse err/service"
            );
        }
        println!("  clickhouse: error rate/service: {ms}ms | 10000 errs, all service 0 | CORRECT");
        let (rows, ms) = timed_ms(|| {
            ch.rows("SELECT service, quantileExact(0.95)(lat) FROM events GROUP BY service")
        });
        for r in &rows {
            assert_eq!(
                ucell(r, 1),
                p95[ucell(r, 0) as usize] as u64,
                "clickhouse p95/service"
            );
        }
        println!(
            "  clickhouse: p95 latency/service: {ms}ms | exact p95 = service+460 × 20 | CORRECT"
        );
    } else {
        println!("  clickhouse: NOT_MEASURED (not opted in)");
    }
    if want_sr {
        let mut conn = sr_connect();
        let (rows, ms) = timed_ms(|| {
            sr_rows(
                &mut conn,
                "SELECT service, day, COUNT(*) FROM aikoql_bench.events GROUP BY service, day",
            )
        });
        assert_eq!(rows.len(), 20 * 12, "starrocks day buckets");
        let total: u64 = rows.iter().map(|r| ucell(r, 2)).sum();
        assert_eq!(total, N_EVT, "starrocks events total");
        for r in &rows {
            if ucell(r, 0) == 0 && ucell(r, 1) == 0 {
                assert_eq!(ucell(r, 2), 4320, "starrocks service 0 day 0 count");
            }
        }
        println!(
            "  starrocks: events/day per service: {ms}ms | {} buckets | total {total} | CORRECT",
            rows.len()
        );
        let (rows, ms) = timed_ms(|| {
            sr_rows(
                &mut conn,
                "SELECT service, COUNT(*) FROM aikoql_bench.events WHERE err = 1 GROUP BY service",
            )
        });
        let total: u64 = rows.iter().map(|r| ucell(r, 1)).sum();
        assert_eq!(total, 10000, "starrocks err total");
        for r in &rows {
            assert_eq!(
                ucell(r, 1),
                errs[ucell(r, 0) as usize] as u64,
                "starrocks err/service"
            );
        }
        println!("  starrocks: error rate/service: {ms}ms | 10000 errs, all service 0 | CORRECT");
        let (rows, ms) = timed_ms(|| {
            sr_rows(&mut conn, "SELECT service, percentile_approx(lat, 0.95) FROM aikoql_bench.events GROUP BY service")
        });
        for r in &rows {
            let got = ucell(r, 1);
            let want = p95[ucell(r, 0) as usize] as u64;
            // percentile_approx is approximate by contract; exact ground
            // truth is service+460 — tolerance, honestly labeled.
            assert!(
                (got as i64 - want as i64).abs() <= 10,
                "starrocks p95/service {got} vs {want}"
            );
        }
        println!("  starrocks: p95 latency/service: {ms}ms | approx p95 = service+460 ±10 × 20 | CORRECT (approx contract)");
    } else {
        println!("  starrocks: NOT_MEASURED (not opted in)");
    }
    println!("  note: events/minute measured as events/day — minute not stored in the loaded schema (honest scope row, dataset.md)");
}

// ── W5-OLAP-003 — high-cardinality GROUP BY ─────────────────────────────

#[test]
fn w5_olap_003_high_cardinality_group_by() {
    let ch = Ch::probe();
    let want_sr = sr_probe();
    if ch.is_none() && !want_sr {
        println!("[W5-OLAP-003] both engines skipped");
        return;
    }
    ensure_loaded(&ch, want_sr);
    let (combos, _, _) = evt_truth();
    println!(
        "[W5-OLAP-003] GROUP BY service, device, region, err, day over 1M events — {} distinct combos",
        combos.len()
    );
    let q = "SELECT service, device, region, err, day, COUNT(*) FROM events GROUP BY service, device, region, err, day";
    if let Some(ch) = &ch {
        let (rows, ms) = timed_ms(|| ch.rows(q));
        assert_eq!(rows.len(), combos.len(), "clickhouse combo count");
        let total: u64 = rows.iter().map(|r| ucell(r, 5)).sum();
        assert_eq!(total, N_EVT, "clickhouse combo total");
        // Spot combos must respect generator coupling: service = device%20,
        // region = device%5, err=1 ⟹ device%100==0 (so err=1 only with
        // service 0, region 0).
        let spot = [
            (0u16, 0u16, 0u16, 1u8, 0u8),
            (3, 123, 3, 0, 3),
            (19, 999, 4, 0, 11),
        ];
        for (s, d, r, e, day) in spot {
            let want = *combos.get(&(s, d, r, e, day)).unwrap() as u64;
            let got = rows
                .iter()
                .find(|row| {
                    ucell(row, 0) == s as u64
                        && ucell(row, 1) == d as u64
                        && ucell(row, 2) == r as u64
                        && ucell(row, 3) == e as u64
                        && ucell(row, 4) == day as u64
                })
                .map(|row| ucell(row, 5))
                .unwrap_or(0);
            assert_eq!(got, want, "clickhouse spot combo ({s},{d},{r},{e},{day})");
        }
        println!(
            "  clickhouse: {ms}ms | {} combos | total {total} | CORRECT",
            rows.len()
        );
    } else {
        println!("  clickhouse: NOT_MEASURED (not opted in)");
    }
    if want_sr {
        let mut conn = sr_connect();
        let (rows, ms) =
            timed_ms(|| sr_rows(&mut conn, &q.replace("events", "aikoql_bench.events")));
        assert_eq!(rows.len(), combos.len(), "starrocks combo count");
        let total: u64 = rows.iter().map(|r| ucell(r, 5)).sum();
        assert_eq!(total, N_EVT, "starrocks combo total");
        // Spot combos must respect generator coupling: service = device%20,
        // region = device%5, err=1 ⟹ device%100==0 (so err=1 only with
        // service 0, region 0).
        let spot = [
            (0u16, 0u16, 0u16, 1u8, 0u8),
            (3, 123, 3, 0, 3),
            (19, 999, 4, 0, 11),
        ];
        for (s, d, r, e, day) in spot {
            let want = *combos.get(&(s, d, r, e, day)).unwrap() as u64;
            let got = rows
                .iter()
                .find(|row| {
                    ucell(row, 0) == s as u64
                        && ucell(row, 1) == d as u64
                        && ucell(row, 2) == r as u64
                        && ucell(row, 3) == e as u64
                        && ucell(row, 4) == day as u64
                })
                .map(|row| ucell(row, 5))
                .unwrap_or(0);
            assert_eq!(got, want, "starrocks spot combo ({s},{d},{r},{e},{day})");
        }
        println!(
            "  starrocks: {ms}ms | {} combos | total {total} | CORRECT",
            rows.len()
        );
    } else {
        println!("  starrocks: NOT_MEASURED (not opted in)");
    }
    println!("  aikoql: NOT_MEASURED — OLAP baseline side of the boundary (plan §4: do not claim superiority here)");
}

// ── W5-OLAP-004 — large multi-table join ────────────────────────────────

#[test]
fn w5_olap_004_multi_table_join() {
    let ch = Ch::probe();
    let want_sr = sr_probe();
    if ch.is_none() && !want_sr {
        println!("[W5-OLAP-004] both engines skipped");
        return;
    }
    ensure_loaded(&ch, want_sr);
    let (_, _, per_tier) = txn_truth();
    println!("[W5-OLAP-004] revenue per tier (txn⋈customers) + device events (events⋈devices)");
    let tier_q = "SELECT c.tier, SUM(t.amount) FROM txn t JOIN customers c ON t.customer_id = c.id GROUP BY c.tier ORDER BY c.tier";
    if let Some(ch) = &ch {
        let (rows, ms) = timed_ms(|| ch.rows(tier_q));
        assert_eq!(rows.len(), 3, "clickhouse tier rows");
        for r in &rows {
            assert_eq!(
                ucell(r, 1),
                per_tier[ucell(r, 0) as usize],
                "clickhouse tier revenue"
            );
        }
        println!("  clickhouse: tier join: {ms}ms | 3 tiers | CORRECT");
        let (rows, ms) = timed_ms(|| {
            ch.rows("SELECT COUNT(*) FROM events e JOIN devices d ON e.device = d.device_id")
        });
        assert_eq!(ucell(&rows[0], 0), N_EVT, "clickhouse device join total");
        let (rows, ms2) = timed_ms(|| {
            ch.rows("SELECT e.device, COUNT(*), MAX(e.lat) FROM events e WHERE e.device IN (0,7,123,499,999) GROUP BY e.device")
        });
        for r in &rows {
            let d = ucell(r, 0);
            assert_eq!(ucell(r, 1), 1000, "clickhouse device {d} events");
            assert_eq!(ucell(r, 2), d % 500, "clickhouse device {d} max lat");
        }
        println!("  clickhouse: device join: {ms}ms/{ms2}ms | 1M matched, spots CORRECT");
    } else {
        println!("  clickhouse: NOT_MEASURED (not opted in)");
    }
    if want_sr {
        let mut conn = sr_connect();
        let (rows, ms) = timed_ms(|| {
            sr_rows(
                &mut conn,
                &tier_q
                    .replace("txn", "aikoql_bench.txn")
                    .replace("customers", "aikoql_bench.customers"),
            )
        });
        assert_eq!(rows.len(), 3, "starrocks tier rows");
        for r in &rows {
            assert_eq!(
                ucell(r, 1),
                per_tier[ucell(r, 0) as usize],
                "starrocks tier revenue"
            );
        }
        println!("  starrocks: tier join: {ms}ms | 3 tiers | CORRECT");
        let (rows, ms) = timed_ms(|| {
            sr_rows(&mut conn, "SELECT COUNT(*) FROM aikoql_bench.events e JOIN aikoql_bench.devices d ON e.device = d.device_id")
        });
        assert_eq!(ucell(&rows[0], 0), N_EVT, "starrocks device join total");
        let (rows, ms2) = timed_ms(|| {
            sr_rows(&mut conn, "SELECT e.device, COUNT(*), MAX(e.lat) FROM aikoql_bench.events e WHERE e.device IN (0,7,123,499,999) GROUP BY e.device")
        });
        for r in &rows {
            let d = ucell(r, 0);
            assert_eq!(ucell(r, 1), 1000, "starrocks device {d} events");
            assert_eq!(ucell(r, 2), d % 500, "starrocks device {d} max lat");
        }
        println!("  starrocks: device join: {ms}ms/{ms2}ms | 1M matched, spots CORRECT");
    } else {
        println!("  starrocks: NOT_MEASURED (not opted in)");
    }
}
