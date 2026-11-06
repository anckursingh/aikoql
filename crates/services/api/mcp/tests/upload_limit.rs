//! `/api/v1/documents` reads its whole body into memory, so a declared body
//! over the cap is refused (413) before it is read.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[test]
fn ul001_an_oversized_document_upload_is_refused_before_it_is_read() {
    let db = std::env::temp_dir().join(format!("aikoql-upl-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&db);
    common::register_temp_path(&db);
    let log = std::env::temp_dir().join(format!("aikoql-upl-{}.log", std::process::id()));
    common::register_temp_path(&log);
    let srv = common::spawn_server(&db, &log, &[("AIKOQL_ADMIN_PASSWORD", "pw")], &[]);
    let health = common::wait_health(srv.port, Instant::now() + Duration::from_secs(60));
    assert!(health.contains(" 200 "), "health: {health}");

    // Only the headers are sent: the answer must not wait for the body.
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(
        b"POST /api/v1/documents HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
          Content-Length: 900000000\r\n\r\n",
    )
    .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let raw = String::from_utf8_lossy(&out);
    assert!(raw.starts_with("HTTP/1.0 413"), "{raw}");
    assert!(raw.contains("at most"), "{raw}");
}
