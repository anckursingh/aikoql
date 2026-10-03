//! Embedded mode (§25): the kernel in-process, no server, no MCP. The same
//! canonical ops as the remote Client (CRUD + vector + lineage) with an
//! admin Subject (ACL parity with the server's TOKEN::admin).

use crate::error::{Error, McpError};
use crate::tools::{FindSimilarParams, KnowledgeObject, RememberParams, Remembered, ScoredKO};
use aikoql_kernel::kernel::{ForgetMode, RememberRequest, SimilarityQuery, Subject};
use aikoql_kernel::kom::{Metadata, PropertyMap, Value as KomValue, KOID};
use aikoql_kernel::{AsyncKernel, Fusion, Kernel, SystemClock};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// An embedded connection: the kernel runs in this process. Not Clone —
/// the kernel behind it is shared; wrap in Arc to share.
pub struct Embedded {
    kernel: AsyncKernel,
    subject: Subject,
}

impl Embedded {
    /// Opens (creating if needed) the database at `path` — the runtime's
    /// one opener, the same engine+clock the server uses.
    pub fn open(path: &str) -> Result<Embedded, Error> {
        let (engine, _admin) = aikoql_runtime::backend::open_engine(Path::new(path))
            .map_err(|e| Error::Kernel(format!("open_engine: {e:?}")))?;
        let kernel = Kernel::open(engine, Arc::new(SystemClock), 0xA9C9)
            .map_err(|e| Error::Kernel(format!("kernel: {e:?}")))?;
        Ok(Embedded {
            kernel: AsyncKernel::new(kernel),
            subject: Subject::with_roles("embedded", &["admin"]),
        })
    }

    /// Commits a knowledge object; with a koid it is a new version.
    pub async fn remember(&self, p: RememberParams) -> Result<Remembered, Error> {
        let metadata = Metadata {
            type_name: p.type_name,
            tenant: None,
            schema_version: 1,
            tags: vec![],
        };
        let mut req = match &p.koid {
            None => RememberRequest::create(self.subject.clone(), metadata),
            Some(k) => {
                let koid = koid(k)?;
                RememberRequest::update(self.subject.clone(), koid, metadata)
            }
        };
        if let Some(props) = p.properties {
            let mut out = PropertyMap::new();
            for (k, v) in props {
                out.insert(k, json_to_value(&v)?);
            }
            req.properties = out;
        }
        let rem = self
            .kernel
            .remember(req)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(Remembered {
            koid: rem.koid.to_string(),
            version: rem.version,
            commit_ts: rem.commit_ts,
        })
    }

    /// Fetches a knowledge object by KOID.
    pub async fn get(&self, koid_str: &str) -> Result<KnowledgeObject, Error> {
        let ko = self
            .kernel
            .get(self.subject.clone(), koid(koid_str)?)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(KnowledgeObject {
            koid: ko.koid.to_string(),
            version: ko.version,
            state: Some(ko.lifecycle.state.to_string()),
            properties: ko
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        })
    }

    /// Tombstones ("tombstone") or legally erases ("erase") a KO.
    pub async fn forget(&self, koid_str: &str, mode: &str) -> Result<Value, Error> {
        let mode = match mode {
            "tombstone" => ForgetMode::Tombstone,
            "erase" => ForgetMode::Erase,
            other => {
                return Err(Error::Mcp(McpError::invalid_argument(format!(
                    "unknown forget mode {other:?}"
                ))))
            }
        };
        let f = self
            .kernel
            .forget(self.subject.clone(), koid(koid_str)?, mode, None, None)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(serde_json::json!({"koid": f.koid.to_string(), "version": f.version}))
    }

    /// Runs hybrid recall in-process (text and/or type filter).
    pub async fn find_similar(&self, p: FindSimilarParams) -> Result<Vec<ScoredKO>, Error> {
        let mut q = SimilarityQuery::new(self.subject.clone(), p.k.unwrap_or(10), Fusion::TextOnly);
        if let Some(text) = p.text {
            q = q.with_text(text);
        }
        let hits = self
            .kernel
            .find_similar(q)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(hits
            .into_iter()
            .map(|s| ScoredKO {
                koid: s.ko.koid.to_string(),
                score: s.score as f64,
                type_name: s.ko.metadata.type_name,
            })
            .collect())
    }

    /// Lineage as {koid, versions}: the version count.
    // ponytail: count-only lineage; the full version/event records ride the
    // remote wire protocol — extend if embedded consumers need them.
    pub async fn trace(&self, koid_str: &str) -> Result<Value, Error> {
        let lineage = self
            .kernel
            .trace(self.subject.clone(), koid(koid_str)?)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(serde_json::json!({
            "koid": lineage.koid.to_string(),
            "versions": lineage.versions.len(),
        }))
    }

    /// Explanation as {koid, version, verified}.
    // ponytail: the minimal explanation shape; the full payload rides the
    // remote wire protocol — extend if embedded consumers need it.
    pub async fn explain(&self, koid_str: &str, version: Option<u64>) -> Result<Value, Error> {
        let e = self
            .kernel
            .explain(self.subject.clone(), koid(koid_str)?, version)
            .await
            .map_err(|e| Error::Kernel(format!("{e:?}")))?;
        Ok(serde_json::json!({
            "koid": e.koid.to_string(),
            "version": e.version,
            "verified": e.verified,
        }))
    }
}

fn koid(s: &str) -> Result<KOID, Error> {
    KOID::from_hex(s).map_err(|e| Error::Kernel(format!("koid: {e:?}")))
}

// Mirror of crates/services/api/mcp/src/helpers.rs json_to_value/value_to_json
// (pub(crate) there — the SDK can't reach across without the server crate).
// The kernel Value's derived serde impl is externally tagged, so plain
// from_value/to_value is NOT wire-compatible: both directions are manual.
fn json_to_value(j: &Value) -> Result<KomValue, Error> {
    Ok(match j {
        Value::Null => KomValue::Null,
        Value::Bool(b) => KomValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                KomValue::Int(i)
            } else if let Some(f) = n.as_f64() {
                KomValue::Float(f)
            } else {
                return Err(Error::Kernel("unsupported number".into()));
            }
        }
        Value::String(s) => KomValue::Text(s.clone()),
        Value::Array(xs) => KomValue::List(
            xs.iter()
                .map(json_to_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(m) => {
            let mut out = BTreeMap::new();
            for (k, v) in m {
                out.insert(k.clone(), json_to_value(v)?);
            }
            KomValue::Map(out)
        }
    })
}

fn value_to_json(v: &KomValue) -> Value {
    match v {
        KomValue::Null => Value::Null,
        KomValue::Bool(b) => Value::Bool(*b),
        KomValue::Int(i) => Value::from(*i),
        KomValue::Float(f) => Value::from(*f),
        KomValue::Text(s) => Value::String(s.clone()),
        KomValue::Bytes(b) => Value::String(format!("{} bytes", b.len())),
        KomValue::List(xs) => Value::Array(xs.iter().map(value_to_json).collect()),
        KomValue::Map(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}
