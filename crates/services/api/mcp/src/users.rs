//! Enterprise multi-user store (Task 3; PROD-2): users and roles as rows
//! of the kernel's system store, under `sys/users/<sub>`, authoritative
//! while a multi-user license is active. Community mode never writes one.
//!
//! PROD-2 moved the table out of a node-local SQLite file: the system
//! store is what replicates, what the storage fence refuses writes on, and
//! what the archive carries — so a promoted standby serves the same users
//! (the OAuth grants' pattern, `sys/oauth/`). A standby refuses every
//! write here with the engine's own "this node is a standby" error.
//!
//! Row value (JSON): `{id, name, roles, created_at, password_hash}` —
//! roles a JSON array, created_at RFC 3339 UTC, password_hash the
//! bootstrap admin's argon2id PHC (null for OIDC-provisioned users). `id`
//! is the creation ordinal the admin API lists by.
//!
//! [`UserStore::import_legacy`] carries a pre-PROD-2 `users.db` over once.

use aikoql_kernel::storage::store::{StorageEngine, WriteBatch};
use serde_json::{json, Value as J};
use std::path::Path;
use std::sync::Arc;

/// The bootstrap admin's `sub` — the same login name the HTTP AuthResolver
/// seeds from AIKOQL_ADMIN_PASSWORD (that resolver is the break-glass
/// credential path, Task 5).
pub(crate) const ADMIN_SUB: &str = "admin";

const P_USER: &str = "sys/users/";

/// What went wrong, by what happened (the repo's classified-error idiom).
#[derive(Debug)]
pub(crate) enum UserError {
    /// The store refused or failed the read/write — a standby or fenced
    /// node, or an I/O failure. Retry against the leader.
    Store(String),
    /// A stored row, a caller argument or the password hashing is wrong.
    Invalid(String),
}

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserError::Store(m) | UserError::Invalid(m) => f.write_str(m),
        }
    }
}

pub(crate) type UserResult<T> = Result<T, UserError>;

fn invalid(msg: impl std::fmt::Display) -> UserError {
    UserError::Invalid(msg.to_string())
}

fn store_err(msg: impl std::fmt::Display) -> UserError {
    UserError::Store(msg.to_string())
}

/// One users row — the shape the admin API and the Studio panel render
/// (Task 8).
#[allow(dead_code)] // the Task 8 contract — read there
pub(crate) struct UserRow {
    pub id: i64,
    pub sub: String,
    pub name: Option<String>,
    pub roles: Vec<String>,
    pub created_at: String,
}

/// A stored row: [`UserRow`] plus the break-glass hash.
struct Stored {
    row: UserRow,
    password_hash: Option<String>,
}

impl Stored {
    fn fresh(id: i64, sub: &str) -> Stored {
        Stored {
            row: UserRow {
                id,
                sub: sub.to_string(),
                name: None,
                roles: Vec::new(),
                created_at: rfc3339_utc_now(),
            },
            password_hash: None,
        }
    }

    fn to_json(&self) -> Vec<u8> {
        json!({"id": self.row.id, "name": self.row.name, "roles": self.row.roles,
               "created_at": self.row.created_at, "password_hash": self.password_hash})
        .to_string()
        .into_bytes()
    }

    fn from_json(sub: &str, raw: &[u8]) -> UserResult<Stored> {
        let bad = || invalid(format!("user row '{sub}' is not a users record"));
        let v: J = serde_json::from_slice(raw).map_err(|_| bad())?;
        let text = |k: &str| v.get(k).and_then(J::as_str).map(String::from);
        let roles = v
            .get("roles")
            .and_then(J::as_array)
            .ok_or_else(bad)?
            .iter()
            .map(|r| r.as_str().map(String::from).ok_or_else(bad))
            .collect::<UserResult<Vec<String>>>()?;
        Ok(Stored {
            row: UserRow {
                id: v.get("id").and_then(J::as_i64).ok_or_else(bad)?,
                sub: sub.to_string(),
                name: text("name"),
                roles,
                created_at: text("created_at").ok_or_else(bad)?,
            },
            password_hash: text("password_hash"),
        })
    }
}

fn user_key(sub: &str) -> Vec<u8> {
    format!("{P_USER}{sub}").into_bytes()
}

pub(crate) struct UserStore {
    store: Arc<dyn StorageEngine>,
}

/// The kernel KMS's argon2id parameters (plan ruling: mirror the kernel's
/// crate and parameters, don't invent).
const ARGON2_MEMORY_KB: u32 = 65536; // 64 MiB
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 4;

/// The bootstrap admin's argon2id PHC: params + salt + hash in one string
/// — what verify_admin_password parses, so verification survives a later
/// params change.
fn hash_password(password: &str) -> UserResult<String> {
    use argon2::password_hash::PasswordHasher;
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).map_err(invalid)?;
    let params = argon2::Params::new(
        ARGON2_MEMORY_KB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(32),
    )
    .map_err(invalid)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt = argon2::password_hash::SaltString::encode_b64(&salt).map_err(invalid)?;
    Ok(argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(invalid)?
        .to_string())
}

impl UserStore {
    /// The users table of `store` — the kernel's system store (the
    /// encrypted one when encryption is on). Nothing is written until a
    /// user is.
    pub(crate) fn new(store: Arc<dyn StorageEngine>) -> Self {
        Self { store }
    }

    fn get(&self, sub: &str) -> UserResult<Option<Stored>> {
        self.store
            .get(&user_key(sub))
            .map_err(store_err)?
            .map(|raw| Stored::from_json(sub, &raw))
            .transpose()
    }

    /// Every row, oldest first.
    fn all(&self) -> UserResult<Vec<Stored>> {
        let mut rows = self
            .store
            .scan(P_USER.as_bytes())
            .map_err(store_err)?
            .iter()
            .map(|(k, v)| {
                let sub = std::str::from_utf8(&k[P_USER.len()..]).map_err(invalid)?;
                Stored::from_json(sub, v)
            })
            .collect::<UserResult<Vec<Stored>>>()?;
        rows.sort_by_key(|s| s.row.id);
        Ok(rows)
    }

    fn put(&self, rows: &[Stored]) -> UserResult<()> {
        let mut batch = WriteBatch::new();
        for s in rows {
            batch.put(user_key(&s.row.sub), s.to_json());
        }
        self.store.write_batch(&batch).map_err(store_err)
    }

    /// The row of `sub`, or a fresh one at the next creation ordinal.
    /// Callers hold the process-wide store mutex (`session::users`), and
    /// only the leader writes — the ordinal cannot be handed out twice.
    fn get_or_fresh(&self, sub: &str) -> UserResult<Stored> {
        if let Some(known) = self.get(sub)? {
            return Ok(known);
        }
        let next = self.all()?.last().map_or(1, |s| s.row.id + 1);
        Ok(Stored::fresh(next, sub))
    }

    /// Any user holding the admin role counts as an admin — the spec's
    /// "seeded or claimed" (an OIDC login may claim admin). A failed check
    /// reads as no admin, and startup then fails on the seed attempt —
    /// never serves with an unverifiable store.
    pub(crate) fn has_admin(&self) -> bool {
        self.all().is_ok_and(|rows| {
            rows.iter()
                .any(|s| s.row.roles.iter().any(|r| r == "admin"))
        })
    }

    /// Seed the bootstrap admin (sub `admin`, role `admin`) from
    /// AIKOQL_ADMIN_PASSWORD on first start. The password is stored as its
    /// argon2id PHC (kernel KMS params — plan ruling); break-glass login
    /// verifies it via [`Self::verify_admin_password`] (Task 5). Ok(false) =
    /// an admin already exists. A demoted `admin` row is repaired (roles
    /// only — the stored hash/name/created_at stay untouched), so a restart
    /// after a PUT-roles demotion seeds instead of failing.
    pub(crate) fn seed_bootstrap_admin(&self, password: &str) -> UserResult<bool> {
        if self.has_admin() {
            return Ok(false);
        }
        let mut row = self.get_or_fresh(ADMIN_SUB)?;
        if row.password_hash.is_none() {
            row.password_hash = Some(hash_password(password)?);
        }
        row.row.roles = vec!["admin".to_string()];
        self.put(&[row])?;
        Ok(true)
    }

    /// Break-glass login check (Task 5): argon2-verify `password` against
    /// the bootstrap admin's stored PHC. Ok(false) when that row carries no
    /// hash — never seeded.
    #[allow(dead_code)] // the Task 5 contract — read there
    pub(crate) fn verify_admin_password(&self, password: &str) -> UserResult<bool> {
        let Some(phc) = self.get(ADMIN_SUB)?.and_then(|s| s.password_hash) else {
            return Ok(false);
        };
        let parsed = argon2::PasswordHash::new(&phc).map_err(invalid)?;
        use argon2::password_hash::PasswordVerifier;
        match argon2::Argon2::default().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(invalid(e)),
        }
    }

    /// Create or refresh an OIDC user: the first login inserts (created_at
    /// now), later logins refresh name + roles from the live claims.
    /// The bootstrap admin's sub is reserved: an upsert there would replace
    /// its roles — refused, loudly.
    #[allow(dead_code)] // the Task 5 contract — read there
    pub(crate) fn upsert_oidc_user(
        &self,
        sub: &str,
        name: Option<&str>,
        roles: &[String],
    ) -> UserResult<()> {
        if sub == ADMIN_SUB {
            return Err(invalid(format!("reserved sub '{ADMIN_SUB}'")));
        }
        let mut row = self.get_or_fresh(sub)?;
        row.row.name = name.map(String::from);
        row.row.roles = roles.to_vec();
        self.put(&[row])
    }

    /// Replace the roles of `sub` (Task 8's PUT /api/users/{sub}/roles);
    /// an unknown `sub` stays absent.
    #[allow(dead_code)] // the Task 8 contract — read there
    pub(crate) fn set_roles(&self, sub: &str, roles: &[String]) -> UserResult<()> {
        let Some(mut row) = self.get(sub)? else {
            return Ok(());
        };
        row.row.roles = roles.to_vec();
        self.put(&[row])
    }

    /// The stored roles of `sub`; None = no such user.
    #[allow(dead_code)] // the Tasks 5/6/8 contract — read there
    pub(crate) fn roles_of(&self, sub: &str) -> UserResult<Option<Vec<String>>> {
        Ok(self.get(sub)?.map(|s| s.row.roles))
    }

    /// Every user, oldest first (Task 8's GET /api/users).
    #[allow(dead_code)] // the Task 8 contract — read there
    pub(crate) fn list_users(&self) -> UserResult<Vec<UserRow>> {
        Ok(self.all()?.into_iter().map(|s| s.row).collect())
    }

    /// PROD-2 migration: carry a pre-PROD-2 SQLite `users.db` into the
    /// store — every row with its id, hash and created_at, in one batch —
    /// then set the file aside as `users.db.migrated`. A store that
    /// already holds users is left as it is (the file is not read): the
    /// store is the authority once it has a row. Ok(n) = rows imported.
    #[allow(dead_code)] // serve's startup path — unused by the test crate
    pub(crate) fn import_legacy(&self, path: &Path) -> UserResult<usize> {
        if !path.exists() {
            return Ok(0);
        }
        if !self.all()?.is_empty() {
            // The store is the authority, so the file is left alone — but
            // its users are dropped, and silently dropping them is the one
            // thing an operator cannot recover from by reading the log.
            let held = read_legacy(path).map_or_else(
                |e| format!("unreadable: {e}"),
                |rows| format!("{} rows", rows.len()),
            );
            tracing::warn!(
                file = %path.display(),
                legacy = %held,
                "legacy users.db skipped: the store already holds users, so its rows \
                 are NOT imported; merge them by hand (admin API) or remove the file"
            );
            return Ok(0);
        }
        let rows = read_legacy(path)
            .map_err(|e| invalid(format!("legacy user store {}: {e}", path.display())))?;
        self.put(&rows)?;
        let aside = path.with_extension("db.migrated");
        std::fs::rename(path, &aside).map_err(|e| {
            store_err(format!(
                "set {} aside as {}: {e}",
                path.display(),
                aside.display()
            ))
        })?;
        Ok(rows.len())
    }
}

/// The rows of a pre-PROD-2 `users(id, sub, name, roles, created_at,
/// password_hash)` SQLite table, oldest first.
fn read_legacy(path: &Path) -> rusqlite::Result<Vec<Stored>> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn
        .prepare("SELECT id, sub, name, roles, created_at, password_hash FROM users ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        let roles: String = row.get(3)?;
        Ok(Stored {
            row: UserRow {
                id: row.get(0)?,
                sub: row.get(1)?,
                name: row.get(2)?,
                roles: serde_json::from_str(&roles)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                created_at: row.get(4)?,
            },
            password_hash: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// Now as RFC 3339 UTC ("YYYY-MM-DDTHH:MM:SSZ"). Civil-date arithmetic via
/// Hinnant's civil_from_days (the inverse of license.rs's days_from_civil)
/// — no chrono dependency for one conversion.
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, sod) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Days since 1970-01-01 → (year, month, day), Hinnant's algorithm.
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

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::storage::store::MemoryEngine;

    fn mem_store() -> UserStore {
        UserStore::new(Arc::new(MemoryEngine::new()))
    }

    #[test]
    fn upsert_refuses_the_reserved_admin_sub() {
        // Final review I-4: the bootstrap admin row (sub "admin") is
        // reserved — an OIDC/API upsert must error and leave it untouched.
        let store = mem_store();
        assert!(store.seed_bootstrap_admin("pw").expect("seed"), "seeded");
        let before = store.roles_of(ADMIN_SUB).expect("roles_of").expect("row");
        let err = store
            .upsert_oidc_user(ADMIN_SUB, Some("IdP admin"), &["viewer".to_string()])
            .expect_err("the reserved sub must be refused");
        assert!(err.to_string().contains("reserved"), "{err}");
        assert_eq!(
            store.roles_of(ADMIN_SUB).expect("roles_of"),
            Some(before),
            "the reserved row must be untouched"
        );
    }

    #[test]
    fn seed_after_demotion_upserts_the_admin_row() {
        // Final review round 2 (seed idempotency): PUT roles can demote
        // sub "admin"; the next restart's seed must restore the role and
        // keep password_hash/name/created_at, so break-glass still verifies.
        let store = mem_store();
        assert!(store.seed_bootstrap_admin("pw").expect("seed"), "seeded");
        store
            .set_roles(ADMIN_SUB, &["viewer".to_string()])
            .expect("demote");
        assert!(!store.has_admin(), "demoted");
        assert!(
            store.seed_bootstrap_admin("other").expect("reseed"),
            "the reseed must repair, not collide"
        );
        assert!(store.has_admin(), "admin restored");
        assert!(
            store.verify_admin_password("pw").expect("verify"),
            "the repair must keep the seeded hash"
        );
    }

    /// PROD-2: a pre-PROD-2 `users.db` is carried into the store once —
    /// ids, roles, created_at and the break-glass hash intact — and set
    /// aside, so the next start does not read it again.
    #[test]
    fn a_legacy_sqlite_store_is_imported_once_and_set_aside() {
        let path = std::env::temp_dir().join(format!(
            "aikoql-users-legacy-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let aside = path.with_extension("db.migrated");
        for p in [&path, &aside] {
            let _ = std::fs::remove_file(p); // a leftover of a killed run
        }
        let phc = hash_password("legacy-pw").expect("hash");
        {
            let conn = rusqlite::Connection::open(&path).expect("legacy file");
            conn.execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, sub TEXT NOT NULL UNIQUE,
                 name TEXT, roles TEXT NOT NULL, created_at TEXT NOT NULL, password_hash TEXT)",
            )
            .expect("legacy schema");
            conn.execute(
                "INSERT INTO users (id, sub, name, roles, created_at, password_hash) VALUES
                 (1, 'admin', NULL, '[\"admin\"]', '2026-01-01T00:00:00Z', ?1),
                 (7, 'alice', 'Alice', '[\"viewer\"]', '2026-02-02T00:00:00Z', NULL)",
                [&phc],
            )
            .expect("legacy rows");
        }
        let store = mem_store();
        assert_eq!(store.import_legacy(&path).expect("import"), 2);
        assert!(!path.exists() && aside.exists(), "the file is set aside");
        let rows = store.list_users().expect("list");
        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.id, r.sub.as_str(), r.created_at.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                (1, "admin", "2026-01-01T00:00:00Z"),
                (7, "alice", "2026-02-02T00:00:00Z")
            ]
        );
        assert_eq!(rows[1].name.as_deref(), Some("Alice"));
        assert!(store.verify_admin_password("legacy-pw").expect("verify"));
        // a new user continues after the imported ordinals
        store.upsert_oidc_user("bob", None, &[]).expect("bob");
        assert_eq!(store.list_users().expect("list")[2].id, 8);
        // nothing left to import; a populated store never reads a file again
        assert_eq!(store.import_legacy(&path).expect("no file"), 0);
        std::fs::rename(&aside, &path).expect("put the file back");
        assert_eq!(store.import_legacy(&path).expect("populated"), 0);
        assert!(path.exists(), "a populated store leaves the file alone");
        let _ = std::fs::remove_file(&path); // test residue
    }
}
