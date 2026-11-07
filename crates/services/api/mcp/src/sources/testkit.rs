//! Shared fixtures for the sources tests (SQLite sources on temp files).

use super::{SourceConfig, SourceKind};
use crate::Kernel;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub(crate) fn kernel() -> Arc<Kernel> {
    Arc::new(
        Kernel::open(
            Arc::new(aikoql_kernel::MemoryEngine::new()),
            Arc::new(aikoql_kernel::ManualClock::new(10_000)),
            0xBEEF,
        )
        .unwrap(),
    )
}

pub(crate) fn tmp_db(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aikoql-sources-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("db.sqlite")
}

pub(crate) fn make(path: &Path, sql: &str) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

pub(crate) fn sqlite_source(name: &str, tenant: Option<&str>) -> SourceConfig {
    SourceConfig {
        name: name.into(),
        kind: SourceKind::Sqlite,
        url_env: "PATH".into(),
        database: None,
        tenant: tenant.map(String::from),
        sample_limit: 100,
        max_containers: 500,
        timeout_secs: 3,
        import_max_rows: 100_000,
    }
}

/// A CRM-like database: customers, orders(customer_id -> customers).
pub(crate) fn crm_db(tag: &str) -> PathBuf {
    let p = tmp_db(tag);
    make(
        &p,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id), total REAL);
         INSERT INTO customers VALUES (1, 'a@x.test');",
    );
    p
}

/// A shop database whose `clients` are the CRM's customers.
pub(crate) fn shop_db(tag: &str) -> PathBuf {
    let p = tmp_db(tag);
    make(
        &p,
        "CREATE TABLE clients (id INTEGER PRIMARY KEY, email_address TEXT NOT NULL);
         INSERT INTO clients VALUES (7, 'b@y.test');",
    );
    p
}

pub(crate) fn conn(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}
