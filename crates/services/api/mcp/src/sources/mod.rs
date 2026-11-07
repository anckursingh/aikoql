//! Source schema discovery: configured external databases, their schema as
//! knowledge, and the ontology drafted from it.

use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceKind {
    Postgres,
    Mongodb,
    Neo4j,
    Sqlite,
    Clickhouse,
}

impl SourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SourceKind::Postgres => "postgres",
            SourceKind::Mongodb => "mongodb",
            SourceKind::Neo4j => "neo4j",
            SourceKind::Sqlite => "sqlite",
            SourceKind::Clickhouse => "clickhouse",
        }
    }
}

fn d_sample() -> u32 {
    100
}
fn d_containers() -> u32 {
    500
}
fn d_timeout() -> u64 {
    15
}
fn d_rows() -> u64 {
    100_000
}

/// One `[[sources]]` entry. The connection string is never in the file: `url_env`
/// NAMES the environment variable that holds it (a path for SQLite).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceConfig {
    pub name: String,
    pub kind: SourceKind,
    pub url_env: String,
    pub database: Option<String>,
    pub tenant: Option<String>,
    #[serde(default = "d_sample")]
    pub sample_limit: u32,
    #[serde(default = "d_containers")]
    pub max_containers: u32,
    #[serde(default = "d_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "d_rows")]
    pub import_max_rows: u64,
}

impl SourceConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let shown: String = self
            .name
            .chars()
            .take(40)
            .flat_map(char::escape_debug)
            .collect();
        let bad = |msg: &str| Err(format!("source '{shown}': {msg}"));
        let plain =
            |t: &str| !t.is_empty() && t.chars().count() <= 128 && !t.chars().any(char::is_control);
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return bad("name must be 1-64 characters of [A-Za-z0-9_-]");
        }
        let env_ok = self.url_env.len() <= 128
            && self
                .url_env
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && self
                .url_env
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !env_ok {
            return bad("url_env must be an environment variable name ([A-Za-z_][A-Za-z0-9_]*)");
        }
        match &self.database {
            Some(d) if !plain(d) => return bad("database must be 1-128 printable characters"),
            None if matches!(self.kind, SourceKind::Mongodb | SourceKind::Clickhouse) => {
                return bad("database is required for this kind")
            }
            _ => {}
        }
        if self.tenant.as_deref().is_some_and(|t| !plain(t)) {
            return bad("tenant must be 1-128 printable characters");
        }
        if !(1..=10_000).contains(&self.sample_limit) {
            return bad("sample_limit must be 1-10000");
        }
        if !(1..=100_000).contains(&self.max_containers) {
            return bad("max_containers must be 1-100000");
        }
        if !(1..=600).contains(&self.timeout_secs) {
            return bad("timeout_secs must be 1-600");
        }
        if !(1..=100_000_000).contains(&self.import_max_rows) {
            return bad("import_max_rows must be 1-100000000");
        }
        Ok(())
    }

    /// The connection string from the environment. The error names the variable,
    /// never a value.
    pub(crate) fn connection(&self) -> Result<String, String> {
        match std::env::var(&self.url_env) {
            Ok(v) if !v.trim().is_empty() => Ok(v),
            Ok(_) => Err(format!(
                "source {}: env var {} is empty",
                self.name, self.url_env
            )),
            Err(_) => Err(format!(
                "source {}: env var {} is not set",
                self.name, self.url_env
            )),
        }
    }
}

/// Validate a whole `[[sources]]` list (per-entry rules plus unique names).
pub(crate) fn validate_sources(sources: &[SourceConfig]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for s in sources {
        s.validate()?;
        if !seen.insert(s.name.as_str()) {
            return Err(format!("duplicate source name {}", s.name));
        }
    }
    Ok(())
}

static SOURCES: OnceLock<Vec<SourceConfig>> = OnceLock::new();

/// Set once at startup from the loaded configuration.
pub(crate) fn install(sources: &[SourceConfig]) {
    let _ = SOURCES.set(sources.to_vec());
}

#[cfg(test)]
static TEST_SOURCES: std::sync::Mutex<Option<Vec<SourceConfig>>> = std::sync::Mutex::new(None);
#[cfg(test)]
static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with `sources` as the configured list; serialised across tests.
#[cfg(test)]
pub(crate) fn with_test_sources<R>(sources: Vec<SourceConfig>, f: impl FnOnce() -> R) -> R {
    let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *TEST_SOURCES.lock().unwrap_or_else(|e| e.into_inner()) = Some(sources);
    let out = f();
    *TEST_SOURCES.lock().unwrap_or_else(|e| e.into_inner()) = None;
    out
}

pub(crate) fn configured() -> Vec<SourceConfig> {
    #[cfg(test)]
    if let Some(v) = TEST_SOURCES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return v;
    }
    SOURCES.get().cloned().unwrap_or_default()
}

/// A source by name, visible only to the tenant it belongs to (a global source
/// to a request with no tenant). "Not found" is the same for a missing source
/// and another tenant's: existence is not leaked.
pub(crate) fn find_source(name: &str, tenant: Option<&str>) -> Result<SourceConfig, String> {
    configured()
        .into_iter()
        .find(|s| s.name == name && s.tenant.as_deref() == tenant)
        .ok_or_else(|| "source not found".to_string())
}

#[cfg(test)]
mod draft_tests {
    use aikoql_ingestion::{definition_from_sources, discover_connector_schema, SourceInput};

    /// The draft is exactly what `tool_save_ontology` decodes and validates.
    #[test]
    fn draft_definition_round_trips_through_ontologydef() {
        let m = discover_connector_schema(
            "postgres",
            "shop",
            &[
                (
                    "customers",
                    &[
                        ("id", "integer", true, false, true),
                        ("email", "text", false, false, false),
                    ],
                ),
                (
                    "orders",
                    &[
                        ("id", "integer", true, false, true),
                        ("customer_id", "integer", false, true, false),
                    ],
                ),
            ],
            &[(
                "orders",
                &["customer_id"],
                "customers",
                &["id"],
                Some("orders_customer_fk"),
            )],
        );
        let draft = definition_from_sources(
            &[SourceInput {
                name: "shop",
                kind: "postgres",
                meta: &m,
            }],
            &[],
        );
        let props = crate::helpers::parse_properties(
            &serde_json::json!({ "properties": draft.definition }),
        )
        .expect("draft JSON parses as properties");
        let def = aikoql_kernel::OntologyDef::from_property_map(&props)
            .expect("draft decodes and validates as an OntologyDef");
        assert_eq!(def.classes.len(), 2);
        assert_eq!(def.mappings.len(), 2);
        assert!(def.relationships.contains_key("orders_customer_fk"));
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn src(name: &str, kind: SourceKind) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            kind,
            url_env: "PATH".into(),
            database: None,
            tenant: None,
            sample_limit: d_sample(),
            max_containers: d_containers(),
            timeout_secs: d_timeout(),
            import_max_rows: d_rows(),
        }
    }

    #[test]
    fn a_plain_entry_validates_and_defaults_are_the_documented_ones() {
        let s = src("crm", SourceKind::Postgres);
        assert!(s.validate().is_ok());
        assert_eq!(
            (
                s.sample_limit,
                s.max_containers,
                s.timeout_secs,
                s.import_max_rows
            ),
            (100, 500, 15, 100_000)
        );
    }

    #[test]
    fn invalid_entries_are_rejected_naming_the_problem() {
        let bad: Vec<(&str, SourceConfig)> = vec![
            ("name chars", src("my source", SourceKind::Sqlite)),
            ("name empty", src("", SourceKind::Sqlite)),
            ("name too long", src(&"a".repeat(65), SourceKind::Sqlite)),
            ("name unicode lookalike", src("с", SourceKind::Sqlite)),
            (
                "url_env empty",
                SourceConfig {
                    url_env: "".into(),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "url_env with =",
                SourceConfig {
                    url_env: "A=B".into(),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "url_env with space",
                SourceConfig {
                    url_env: "A B".into(),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "url_env nul",
                SourceConfig {
                    url_env: "A\0B".into(),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "sample 0",
                SourceConfig {
                    sample_limit: 0,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "sample too big",
                SourceConfig {
                    sample_limit: 10_001,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "containers 0",
                SourceConfig {
                    max_containers: 0,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "timeout 0",
                SourceConfig {
                    timeout_secs: 0,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "timeout huge",
                SourceConfig {
                    timeout_secs: 601,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "rows 0",
                SourceConfig {
                    import_max_rows: 0,
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            ("mongodb needs a database", src("a", SourceKind::Mongodb)),
            (
                "clickhouse needs a database",
                src("a", SourceKind::Clickhouse),
            ),
            (
                "database control char",
                SourceConfig {
                    database: Some("d\nb".into()),
                    ..src("a", SourceKind::Mongodb)
                },
            ),
            (
                "tenant empty",
                SourceConfig {
                    tenant: Some("".into()),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
            (
                "tenant control char",
                SourceConfig {
                    tenant: Some("t\0".into()),
                    ..src("a", SourceKind::Sqlite)
                },
            ),
        ];
        for (what, s) in bad {
            assert!(s.validate().is_err(), "should reject: {what}");
        }
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let list = vec![src("a", SourceKind::Sqlite), src("a", SourceKind::Postgres)];
        let e = validate_sources(&list).unwrap_err();
        assert!(e.contains("duplicate") && e.contains('a'), "{e}");
    }

    #[test]
    fn connection_reads_the_env_var_and_errors_name_the_variable_not_a_value() {
        // PATH is always set; the test only reads the environment
        let s = src("a", SourceKind::Sqlite);
        assert_eq!(s.connection().unwrap(), std::env::var("PATH").unwrap());
        let missing = SourceConfig {
            url_env: "AIKOQL_SOURCES_TEST_NEVER_SET".into(),
            ..s
        };
        let e = missing.connection().unwrap_err();
        assert!(
            e.contains("AIKOQL_SOURCES_TEST_NEVER_SET") && e.contains("not set"),
            "{e}"
        );
    }

    #[test]
    fn find_source_is_tenant_strict_and_does_not_leak_existence() {
        let list = vec![
            SourceConfig {
                tenant: Some("acme".into()),
                ..src("crm", SourceKind::Sqlite)
            },
            src("shared", SourceKind::Sqlite),
        ];
        with_test_sources(list, || {
            assert_eq!(find_source("crm", Some("acme")).unwrap().name, "crm");
            assert_eq!(find_source("shared", None).unwrap().name, "shared");
            let other = find_source("crm", Some("globex")).unwrap_err();
            let none = find_source("crm", None).unwrap_err();
            let missing = find_source("nope", Some("acme")).unwrap_err();
            assert_eq!(other, none);
            assert_eq!(
                other.replace("crm", "X"),
                missing.replace("nope", "X"),
                "same wording for absent and foreign"
            );
            assert!(
                find_source("shared", Some("acme")).is_err(),
                "a tenant request cannot use a global source"
            );
        });
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[(self.next() % xs.len() as u64) as usize]
        }
    }

    fn fuzz_iters() -> u64 {
        std::env::var("AIKOQL_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_000)
    }

    #[test]
    fn sources_config_fuzz_never_panics_and_valid_names_are_safe() {
        let strings = [
            "", "a", "crm", "с", "a b", "a\0b", "x\ny", "💥", "../x", "é", "A=B", "PATH",
        ];
        let kinds = [
            "postgres",
            "mongodb",
            "neo4j",
            "sqlite",
            "clickhouse",
            "oracle",
            "",
            "PostgreS",
        ];
        for seed in 1..=fuzz_iters() {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut obj = serde_json::Map::new();
            let mut maybe = |k: &str, v: serde_json::Value, r: &mut Rng| {
                if r.next() % 5 != 0 {
                    obj.insert(k.into(), v);
                }
            };
            maybe("name", serde_json::json!(r.pick(&strings)), &mut r);
            maybe("kind", serde_json::json!(r.pick(&kinds)), &mut r);
            maybe("url_env", serde_json::json!(r.pick(&strings)), &mut r);
            maybe("database", serde_json::json!(r.pick(&strings)), &mut r);
            maybe("tenant", serde_json::json!(r.pick(&strings)), &mut r);
            let ints = [
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(u64::MAX),
                serde_json::json!(-5),
                serde_json::json!("x"),
                serde_json::json!(10_001),
                serde_json::json!(15),
            ];
            for k in [
                "sample_limit",
                "max_containers",
                "timeout_secs",
                "import_max_rows",
            ] {
                let v = ints[(r.next() % ints.len() as u64) as usize].clone();
                maybe(k, v, &mut r);
            }
            if r.next() % 7 == 0 {
                obj.insert("surprise".into(), serde_json::json!(1));
            }
            let Ok(cfg) = serde_json::from_value::<SourceConfig>(serde_json::Value::Object(obj))
            else {
                continue;
            };
            if cfg.validate().is_ok() {
                assert!(
                    !cfg.name.is_empty()
                        && cfg.name.len() <= 64
                        && cfg
                            .name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                    "seed {seed}: accepted name {:?}",
                    cfg.name
                );
                assert!(
                    (1..=10_000).contains(&cfg.sample_limit) && cfg.timeout_secs >= 1,
                    "seed {seed}"
                );
                assert!(
                    !cfg.url_env.contains(['=', '\0', ' ']) && !cfg.url_env.is_empty(),
                    "seed {seed}"
                );
                let _ = cfg.connection(); // never panics
            }
        }
    }
}
