//! Typed wrappers — the canonical Database API surface over the MCP tools.
//! Mirrors crates/sdk/go/tools.go (the server's tool registry is the
//! schema source); the 19 conformance ops map onto these plus Tx and
//! query_stream. Anything without a wrapper here has call_tool.

use crate::client::Client;
use crate::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// RememberParams is one knowledge-object commit (create or new version).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RememberParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub koid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<Map<String, Value>>,
}

/// The server's answer to remember.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remembered {
    pub koid: String,
    pub version: u64,
    pub commit_ts: u64,
}

/// A fetched KO (the wire shape — the canonical subset of the kernel's).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeObject {
    pub koid: String,
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

/// FindSimilarParams is a hybrid recall query (vector + text + filters,
/// RRF/weighted fusion).
#[derive(Debug, Clone, Default, Serialize)]
pub struct FindSimilarParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_freshness_ms: Option<i64>,
}

/// One recall hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredKO {
    pub koid: String,
    pub score: f64,
    pub type_name: String,
}

#[derive(Debug, Deserialize)]
struct FindSimilarResult {
    results: Vec<ScoredKO>,
}

/// The server's metrics payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub journal_seq: u64,
    pub total_objects: i64,
    pub active_objects: i64,
    pub uptime_seconds: f64,
    #[serde(default)]
    pub by_lifecycle: Map<String, Value>,
    #[serde(default)]
    pub by_type: Map<String, Value>,
}

fn args(pairs: &[(&str, Value)]) -> Option<Value> {
    if pairs.is_empty() {
        return None;
    }
    Some(Value::Object(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    ))
}

impl Client {
    /// Commits a knowledge object (or a new version of one).
    pub async fn remember(&self, p: RememberParams) -> Result<Remembered, Error> {
        let raw = self
            .call_tool(
                "remember",
                Some(serde_json::to_value(p).map_err(Error::Json)?),
            )
            .await?;
        serde_json::from_value(raw).map_err(Error::Json)
    }

    /// Fetches a knowledge object by KOID.
    pub async fn get(&self, koid: &str, subject: &str) -> Result<KnowledgeObject, Error> {
        let mut pairs = vec![("koid", Value::String(koid.into()))];
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        let raw = self.call_tool("get", args(&pairs)).await?;
        serde_json::from_value(raw).map_err(Error::Json)
    }

    /// Tombstones ("tombstone") or legally erases ("erase") a knowledge
    /// object, audit-preserving.
    pub async fn forget(&self, koid: &str, mode: &str, subject: &str) -> Result<Value, Error> {
        let mut pairs = vec![
            ("koid", Value::String(koid.into())),
            ("mode", Value::String(mode.into())),
        ];
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("forget", args(&pairs)).await
    }

    /// Runs hybrid recall and returns the scored hits.
    pub async fn find_similar(&self, p: FindSimilarParams) -> Result<Vec<ScoredKO>, Error> {
        let raw = self
            .call_tool(
                "find_similar",
                Some(serde_json::to_value(p).map_err(Error::Json)?),
            )
            .await?;
        let res: FindSimilarResult = serde_json::from_value(raw).map_err(Error::Json)?;
        Ok(res.results)
    }

    /// Runs an AikoQL query and returns the raw result rows.
    pub async fn aikoql(&self, query: &str, subject: &str) -> Result<Value, Error> {
        let mut pairs = vec![("query", Value::String(query.into()))];
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("aikoql", args(&pairs)).await
    }

    /// Links two knowledge objects.
    pub async fn relate(
        &self,
        from_koid: &str,
        to_koid: &str,
        rel_type: &str,
        subject: &str,
    ) -> Result<Value, Error> {
        let mut pairs = vec![
            ("from", Value::String(from_koid.into())),
            ("to", Value::String(to_koid.into())),
            ("rel_type", Value::String(rel_type.into())),
        ];
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("relate", args(&pairs)).await
    }

    /// Walks the relationship graph from a KOID.
    pub async fn traverse(
        &self,
        koid: &str,
        rel_type: &str,
        subject: &str,
        depth: i64,
    ) -> Result<Value, Error> {
        let mut pairs = vec![
            ("koid", Value::String(koid.into())),
            ("depth", Value::from(depth)),
        ];
        if !rel_type.is_empty() {
            pairs.push(("rel_type", Value::String(rel_type.into())));
        }
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("traverse", args(&pairs)).await
    }

    /// Returns the server health payload.
    pub async fn health(&self) -> Result<Value, Error> {
        self.call_tool("health", None).await
    }

    /// Returns the schema-discovery payload.
    pub async fn discover_schema(&self) -> Result<Value, Error> {
        self.call_tool("discover_schema", None).await
    }

    /// Returns the server's metrics.
    pub async fn metrics(&self) -> Result<Metrics, Error> {
        let raw = self.call_tool("metrics", None).await?;
        serde_json::from_value(raw).map_err(Error::Json)
    }

    /// Returns the full lineage of a fact (versions + events).
    pub async fn trace(&self, koid: &str, subject: &str) -> Result<Value, Error> {
        let mut pairs = vec![("koid", Value::String(koid.into()))];
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("trace", args(&pairs)).await
    }

    /// Returns the explanation payload for a KO version.
    pub async fn explain(
        &self,
        koid: &str,
        subject: &str,
        version: Option<u64>,
    ) -> Result<Value, Error> {
        let mut pairs = vec![("koid", Value::String(koid.into()))];
        if let Some(v) = version {
            pairs.push(("version", Value::from(v)));
        }
        if !subject.is_empty() {
            pairs.push(("subject", Value::String(subject.into())));
        }
        self.call_tool("explain", args(&pairs)).await
    }
}
