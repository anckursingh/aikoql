//! Enterprise multi-user store (Task 3): the SQLite (rusqlite) users table
//! in the data dir, authoritative for users/roles while a multi-user
//! license is active. Community mode never opens it — no file is created.
//!
//! Schema (pinned by the plan): `users(id INTEGER PRIMARY KEY,
//! sub TEXT NOT NULL UNIQUE, name TEXT, roles TEXT NOT NULL,
//! created_at TEXT NOT NULL)` — roles is a JSON array, created_at RFC 3339
//! UTC — plus, by controller ruling, `password_hash TEXT` (the bootstrap
//! admin's argon2id PHC; NULL for OIDC-provisioned users). Consumed by
//! Tasks 5, 6, 8.

use rusqlite::Connection;
use std::path::Path;

/// The bootstrap admin's `sub` — the same login name the HTTP AuthResolver
/// seeds from AIKOQL_ADMIN_PASSWORD (that resolver is the break-glass
/// credential path, Task 5).
pub(crate) const ADMIN_SUB: &str = "admin";

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

pub(crate) struct UserStore {
    conn: Connection,
}

/// The kernel KMS's argon2id parameters (plan ruling: mirror the kernel's
/// crate and parameters, don't invent).
const ARGON2_MEMORY_KB: u32 = 65536; // 64 MiB
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 4;

/// Non-SQL failures (serde, argon2, the OS RNG) ride the store's uniform
/// `rusqlite::Result` — the only way they can fail here is converting a
/// value for SQL, and this is the vehicle. (argon2's error type predates
/// std::error::Error, so a message box is the honest mapping.)
fn to_sql_error(msg: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(msg.to_string())))
}

impl UserStore {
    /// Open (or create) the store at `path`, creating the table if absent.
    pub(crate) fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                sub TEXT NOT NULL UNIQUE,
                name TEXT,
                roles TEXT NOT NULL,
                created_at TEXT NOT NULL,
                password_hash TEXT
            )",
        )?;
        Ok(Self { conn })
    }

    /// Any user holding the admin role counts as an admin — the spec's
    /// "seeded or claimed" (an OIDC login may claim admin). A failed check
    /// reads as no admin, and startup then fails on the seed attempt —
    /// never serves with an unverifiable store.
    pub(crate) fn has_admin(&self) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE roles LIKE '%\"admin\"%')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }

    /// Seed the bootstrap admin (sub `admin`, role `admin`) from
    /// AIKOQL_ADMIN_PASSWORD on first start. The password is stored as its
    /// argon2id PHC (kernel KMS params — plan ruling) in `password_hash`;
    /// break-glass login verifies it via [`Self::verify_admin_password`]
    /// (Task 5). Ok(false) = an admin already exists. The insert upserts on
    /// sub: a demoted `admin` row is repaired (roles only — the stored
    /// hash/name/created_at stay untouched), so a restart after a PUT-roles
    /// demotion seeds instead of exit-1-ing on UNIQUE(sub).
    pub(crate) fn seed_bootstrap_admin(&self, password: &str) -> rusqlite::Result<bool> {
        if self.has_admin() {
            return Ok(false);
        }
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).map_err(to_sql_error)?;
        let params = argon2::Params::new(
            ARGON2_MEMORY_KB,
            ARGON2_ITERATIONS,
            ARGON2_PARALLELISM,
            Some(32),
        )
        .map_err(to_sql_error)?;
        let argon =
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        // PHC: params + salt + hash in one string — what verify_admin_password
        // parses, so verification survives a later params change
        use argon2::password_hash::PasswordHasher;
        let salt_string =
            argon2::password_hash::SaltString::encode_b64(&salt).map_err(to_sql_error)?;
        let phc = argon
            .hash_password(password.as_bytes(), &salt_string)
            .map_err(to_sql_error)?;
        let roles_json = serde_json::to_string(&["admin".to_string()]).map_err(to_sql_error)?;
        self.conn.execute(
            "INSERT INTO users (sub, name, roles, created_at, password_hash)
             VALUES (?1, NULL, ?2, ?3, ?4)
             ON CONFLICT(sub) DO UPDATE SET roles = excluded.roles",
            rusqlite::params![ADMIN_SUB, roles_json, rfc3339_utc_now(), phc.to_string()],
        )?;
        Ok(true)
    }

    /// Break-glass login check (Task 5): argon2-verify `password` against
    /// the bootstrap admin's stored PHC. Ok(false) when that row carries no
    /// hash — never seeded, or an OIDC user took sub `admin`.
    #[allow(dead_code)] // the Task 5 contract — read there
    pub(crate) fn verify_admin_password(&self, password: &str) -> rusqlite::Result<bool> {
        use rusqlite::OptionalExtension;
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT password_hash FROM users WHERE sub = ?1",
                [ADMIN_SUB],
                |row| row.get(0),
            )
            .optional()?;
        let Some(Some(phc)) = stored else {
            return Ok(false);
        };
        let parsed = argon2::PasswordHash::new(&phc).map_err(to_sql_error)?;
        use argon2::password_hash::PasswordVerifier;
        match argon2::Argon2::default().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(to_sql_error(e)),
        }
    }

    /// Create or refresh an OIDC user: the first login inserts (created_at
    /// now), later logins refresh name + roles from the live claims.
    /// The bootstrap admin's sub is reserved: an upsert there would replace
    /// its roles and brick the next restart's seed INSERT (UNIQUE(sub)) —
    /// refused, loudly.
    #[allow(dead_code)] // the Task 5 contract — read there
    pub(crate) fn upsert_oidc_user(
        &self,
        sub: &str,
        name: Option<&str>,
        roles: &[String],
    ) -> rusqlite::Result<()> {
        if sub == ADMIN_SUB {
            return Err(to_sql_error(format!("reserved sub '{ADMIN_SUB}'")));
        }
        let roles_json = serde_json::to_string(roles).map_err(to_sql_error)?;
        self.conn.execute(
            "INSERT INTO users (sub, name, roles, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(sub) DO UPDATE SET name = excluded.name, roles = excluded.roles",
            rusqlite::params![sub, name, roles_json, rfc3339_utc_now()],
        )?;
        Ok(())
    }

    /// Replace the roles of `sub` (Task 8's PUT /api/users/{sub}/roles).
    #[allow(dead_code)] // the Task 8 contract — read there
    pub(crate) fn set_roles(&self, sub: &str, roles: &[String]) -> rusqlite::Result<()> {
        let roles_json = serde_json::to_string(roles).map_err(to_sql_error)?;
        self.conn.execute(
            "UPDATE users SET roles = ?1 WHERE sub = ?2",
            rusqlite::params![roles_json, sub],
        )?;
        Ok(())
    }

    /// The stored roles of `sub`; None = no such user.
    #[allow(dead_code)] // the Tasks 5/6/8 contract — read there
    pub(crate) fn roles_of(&self, sub: &str) -> rusqlite::Result<Option<Vec<String>>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row("SELECT roles FROM users WHERE sub = ?1", [sub], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .map(|raw| serde_json::from_str(&raw).map_err(to_sql_error))
            .transpose()
    }

    /// Every user, oldest first (Task 8's GET /api/users).
    #[allow(dead_code)] // the Task 8 contract — read there
    pub(crate) fn list_users(&self) -> rusqlite::Result<Vec<UserRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, sub, name, roles, created_at FROM users ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let roles: String = row.get(3)?;
            Ok(UserRow {
                id: row.get(0)?,
                sub: row.get(1)?,
                name: row.get(2)?,
                roles: serde_json::from_str(&roles).map_err(to_sql_error)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }
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

    #[test]
    fn upsert_refuses_the_reserved_admin_sub() {
        // Final review I-4: the bootstrap admin row (sub "admin") is
        // reserved — an OIDC/API upsert must error and leave it untouched
        // (an overwrite would brick the next restart's seed INSERT).
        let path =
            std::env::temp_dir().join(format!("aikoql-users-reserved-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = UserStore::open(&path).expect("open store");
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
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn seed_after_demotion_upserts_the_admin_row() {
        // Final review round 2 (seed idempotency): PUT roles can demote
        // sub "admin", and the next restart's seed then INSERTs a row that
        // exists — UNIQUE(sub) would exit-1 the boot. The seed must upsert
        // the roles back; password_hash/name/created_at stay untouched on
        // the conflict path, so break-glass still verifies.
        let path =
            std::env::temp_dir().join(format!("aikoql-users-reseed-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = UserStore::open(&path).expect("open store");
        assert!(store.seed_bootstrap_admin("pw").expect("seed"), "seeded");
        store
            .set_roles(ADMIN_SUB, &["viewer".to_string()])
            .expect("demote");
        assert!(!store.has_admin(), "demoted");
        assert!(
            store.seed_bootstrap_admin("pw").expect("reseed"),
            "the reseed must upsert, not collide"
        );
        assert!(store.has_admin(), "admin restored");
        assert!(
            store.verify_admin_password("pw").expect("verify"),
            "the conflict path must keep the seeded hash"
        );
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
