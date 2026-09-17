//! Python SDK for the Aikoql Knowledge Kernel.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::useless_conversion)]
//!
//! Exposes a minimal, synchronous `aikoql` class backed by the durable
//! kernel. The LangGraph checkpointer wrapper lives in pure Python on top of
//! these primitives (`aikoql.checkpointer`).

use aikoql_graph::{GraphEngineApi, RelateRequest, TraverseQuery};
use aikoql_kernel::storage::store::StorageEngine;
use aikoql_kernel::{
    Fusion, IndexMaintainerApi, IndexStatusKind, Kernel, KnowledgeContext, Metadata, RedbEngine,
    RememberRequest, ScoredKO, SemanticBlock, SimilarityQuery, Subject, SystemClock, TextIndex,
    Value, VectorIndex, KOID,
};
use aikoql_scheduler::IndexMaintainer;
use aikoql_storage::AikoqlStorageEngine;
use aikoql_storage_v2::AikoqlStorageEngineV2;
use aikoql_vector::{HnswVectorIndex, TantivyTextIndex};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPyObjectExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

fn to_pyerr(e: aikoql_kernel::KError) -> PyErr {
    PyRuntimeError::new_err(format!("{}", e))
}

fn value_from_py(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        Ok(Value::Null)
    } else if let Ok(b) = obj.extract::<bool>() {
        Ok(Value::Bool(b))
    } else if let Ok(i) = obj.extract::<i64>() {
        Ok(Value::Int(i))
    } else if let Ok(f) = obj.extract::<f64>() {
        Ok(Value::Float(f))
    } else if let Ok(s) = obj.extract::<String>() {
        Ok(Value::Text(s))
    } else if let Ok(b) = obj.extract::<Vec<u8>>() {
        Ok(Value::Bytes(b))
    } else if let Ok(list) = obj.clone().cast::<PyList>() {
        let mut v = Vec::new();
        for item in list.iter() {
            v.push(value_from_py(&item)?);
        }
        Ok(Value::List(v))
    } else if let Ok(dict) = obj.clone().cast::<PyDict>() {
        let mut m = BTreeMap::new();
        for (k, val) in dict.iter() {
            let key: String = k.extract()?;
            m.insert(key, value_from_py(&val)?);
        }
        Ok(Value::Map(m))
    } else {
        Err(PyValueError::new_err(
            "unsupported Python value type for aikoql property",
        ))
    }
}

fn value_to_py(py: Python<'_>, v: &Value) -> Py<PyAny> {
    match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py_any(py).unwrap(),
        Value::Int(i) => i.into_py_any(py).unwrap(),
        Value::Float(f) => f.into_py_any(py).unwrap(),
        Value::Text(s) => s.clone().into_py_any(py).unwrap(),
        Value::Bytes(b) => b.clone().into_py_any(py).unwrap(),
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_py(py, item)).unwrap();
            }
            list.into_py_any(py).unwrap()
        }
        Value::Map(m) => {
            let dict = PyDict::new(py);
            for (k, val) in m.iter() {
                dict.set_item(k, value_to_py(py, val)).unwrap();
            }
            dict.into_py_any(py).unwrap()
        }
    }
}

fn optional_string(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match dict.get_item(key) {
        Ok(Some(v)) if !v.is_none() => Ok(Some(v.extract::<String>()?)),
        _ => Ok(None),
    }
}

fn optional_f32(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<f32>> {
    match dict.get_item(key) {
        Ok(Some(v)) if !v.is_none() => Ok(Some(v.extract::<f32>()?)),
        _ => Ok(None),
    }
}

fn optional_embedding(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<Vec<f32>>> {
    match dict.get_item(key) {
        Ok(Some(v)) if !v.is_none() => Ok(Some(v.extract::<Vec<f32>>()?)),
        _ => Ok(None),
    }
}

fn semantic_from_py(sem: &Bound<'_, PyDict>) -> PyResult<SemanticBlock> {
    Ok(SemanticBlock {
        embedding_model: optional_string(sem, "embedding_model")?,
        embedding: optional_embedding(sem, "embedding")?,
        confidence: optional_f32(sem, "confidence")?,
        source: optional_string(sem, "source")?,
        summary: optional_string(sem, "summary")?,
    })
}

fn ko_to_py(py: Python<'_>, ko: &aikoql_kernel::KnowledgeObject) -> Py<PyAny> {
    let dict = PyDict::new(py);
    dict.set_item("koid", ko.koid.to_hex()).unwrap();
    dict.set_item("version", ko.version).unwrap();
    dict.set_item("commit_ts", ko.commit_ts).unwrap();
    dict.set_item("type_name", ko.metadata.type_name.clone())
        .unwrap();
    dict.set_item("schema_version", ko.metadata.schema_version)
        .unwrap();
    dict.set_item("properties", props_to_py(py, &ko.properties))
        .unwrap();
    dict.set_item("lifecycle", ko.lifecycle.state.to_string())
        .unwrap();
    dict.set_item("origin", format!("{:?}", ko.lifecycle.origin))
        .unwrap();
    dict.into_py_any(py).unwrap()
}

fn props_to_py(py: Python<'_>, props: &BTreeMap<String, Value>) -> Py<PyAny> {
    let dict = PyDict::new(py);
    for (k, v) in props.iter() {
        dict.set_item(k, value_to_py(py, v)).unwrap();
    }
    dict.into_py_any(py).unwrap()
}

fn scored_ko_to_py(py: Python<'_>, s: &ScoredKO) -> Py<PyAny> {
    let dict = PyDict::new(py);
    dict.set_item("ko", ko_to_py(py, &s.ko)).unwrap();
    dict.set_item("score", s.score).unwrap();
    dict.set_item("index_lag_ms", s.index_lag_ms).unwrap();
    dict.into_py_any(py).unwrap()
}

#[pyclass(name = "aikoql")]
pub struct Aikoql {
    inner: Arc<Kernel>,
    /// P5-M17b (ND-14): the embedded production property-index maintainer.
    /// The maintainer's thread holds the inner state + a kernel handle, not
    /// this Arc — dropping Aikoql stops it cleanly.
    maintainer: Arc<IndexMaintainer>,
    /// P5-M22 (P1-15): the checkpoint directory (`{path}.ckpt`) — the open
    /// resumes from it, the Drop writes it.
    checkpoint_dir: std::path::PathBuf,
}

/// P5-M22 (P1-15): resume from `ckpt_dir` when a COMPLETE checkpoint is
/// there — restart cost ∝ the events after it — and start fresh with a full
/// replay otherwise. ANY load failure (a torn pair, P1-14; a foreign water,
/// P1-15) falls back to a fresh start, never to an unavailable index.
fn start_maintainer(
    kernel: &Kernel,
    ckpt_dir: &std::path::Path,
) -> aikoql_kernel::KResult<Arc<IndexMaintainer>> {
    if let Some(water) = IndexMaintainer::checkpoint_water(ckpt_dir)? {
        if let (Ok(v), Ok(t)) = (
            HnswVectorIndex::load(&ckpt_dir.join("vectors")),
            TantivyTextIndex::load(&ckpt_dir.join("text")),
        ) {
            let vectors: Arc<dyn VectorIndex> = Arc::new(v);
            let text: Arc<dyn TextIndex> = Arc::new(t);
            if let Ok(m) = IndexMaintainer::start_at(kernel, vectors, text, Some(water)) {
                return Ok(m);
            }
        }
    }
    IndexMaintainer::start(
        kernel,
        Arc::new(HnswVectorIndex::new(0, 10_000)),
        Arc::new(TantivyTextIndex::new()?),
    )
}

impl Drop for Aikoql {
    fn drop(&mut self) {
        // P5-M22 (P1-15): checkpoint on close — the next open resumes
        // instead of replaying the whole journal. Best-effort: a failed
        // checkpoint only costs the next open a full replay.
        let _ = self.maintainer.checkpoint(&self.checkpoint_dir);
    }
}

#[pymethods]
impl Aikoql {
    #[new]
    #[pyo3(signature = (path, salt = 0, backend = "aikoql-v2"))]
    fn new(path: &str, salt: u64, backend: &str) -> PyResult<Self> {
        // Default: aikoql-v2, the ratified production default (2026-09-07
        // ADR). "aikoql" and "redb" open existing databases; the migration
        // path is the REC-002 backup/restore flow.
        let engine: Arc<dyn StorageEngine> = match backend {
            "aikoql-v2" => Arc::new(AikoqlStorageEngineV2::open(path).map_err(to_pyerr)?),
            "aikoql" => Arc::new(AikoqlStorageEngine::open(path).map_err(to_pyerr)?),
            "redb" => Arc::new(RedbEngine::open(path).map_err(to_pyerr)?),
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown backend {other:?}: use \"aikoql-v2\", \"aikoql\" or \"redb\""
                )))
            }
        };
        let kernel = Kernel::open(engine, Arc::new(SystemClock), salt).map_err(to_pyerr)?;
        // P5-M18: real ANN/BM25 indexes behind a FULL journal replay — a
        // live-only maintainer (M17b) leaves the vector index permanently
        // empty, and the candidate-driven coordinator ranks only what the
        // index nominates (empty = no hits). The replay commits in batches
        // (one Tantivy commit per 64 events), so open stays proportional;
        // the harness's per-cell opens pay it outside the timed ops. The
        // HNSW adopts the first vector's dim (SDK callers send arbitrary
        // dims) and its capacity is an allocator hint only.
        //
        // P5-M22 (P1-15): the replay is paid once — a prior Drop left a
        // checkpoint, and the open resumes from it instead.
        let checkpoint_dir = std::path::PathBuf::from(format!("{path}.ckpt"));
        let maintainer = start_maintainer(&kernel, &checkpoint_dir).map_err(to_pyerr)?;
        kernel.attach_indexes(maintainer.clone());
        Ok(Aikoql {
            inner: Arc::new(kernel),
            maintainer,
            checkpoint_dir,
        })
    }

    #[pyo3(signature = (subject, type_name, properties, semantic = None, roles = None, koid = None))]
    fn remember(
        &self,
        py: Python<'_>,
        subject: &str,
        type_name: &str,
        properties: &Bound<'_, PyDict>,
        semantic: Option<&Bound<'_, PyDict>>,
        roles: Option<Vec<String>>,
        koid: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        // Extract all Python data while the GIL is held; the closure passed to
        // `allow_threads` must be `Send`, so it cannot borrow `Bound` handles.
        let roles = roles.unwrap_or_default();
        let mut prop_map: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in properties.iter() {
            let key: String = k.extract()?;
            prop_map.insert(key, value_from_py(&v)?);
        }
        let semantic = semantic.map(semantic_from_py).transpose()?;
        let type_name = type_name.to_string();
        // koid present = update (MCP parity); the kernel update REPLACES the
        // property map, so callers must restate every field they keep.
        let koid = koid
            .map(KOID::from_hex)
            .transpose()
            .map_err(|e| PyValueError::new_err(format!("{}", e)))?;

        let res = py.detach(move || {
            let subject = Subject::with_roles(
                subject,
                &roles.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            );
            let metadata = Metadata {
                type_name,
                tenant: None,
                schema_version: 1,
                tags: vec![],
            };
            let mut req = match koid {
                Some(k) => RememberRequest::update(subject, k, metadata),
                None => RememberRequest::create(subject, metadata),
            };
            req.properties = prop_map;
            req.semantic = semantic;
            self.inner.remember(req).map_err(to_pyerr)
        })?;
        let dict = PyDict::new(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py_any(py).unwrap())
    }

    fn get(&self, py: Python<'_>, subject: &str, koid: &str) -> PyResult<Py<PyAny>> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let ko = py.detach(move || {
            let subj = Subject::new(&subject);
            self.inner.get(&subj, &koid).map_err(to_pyerr)
        })?;
        Ok(ko_to_py(py, &ko))
    }

    fn forget(&self, py: Python<'_>, subject: &str, koid: &str) -> PyResult<Py<PyAny>> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let res = py.detach(move || {
            let subj = Subject::new(&subject);
            self.inner
                .forget(
                    &subj,
                    &koid,
                    aikoql_kernel::ForgetMode::Tombstone,
                    None,
                    None,
                )
                .map_err(to_pyerr)
        })?;
        let dict = PyDict::new(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py_any(py).unwrap())
    }

    #[pyo3(signature = (subject, text = None, vector = None, embedding_model = None, k = 5, fusion = "rrf"))]
    fn find_similar(
        &self,
        py: Python<'_>,
        subject: &str,
        text: Option<String>,
        vector: Option<Vec<f32>>,
        embedding_model: Option<String>,
        k: usize,
        fusion: &str,
    ) -> PyResult<Py<PyAny>> {
        let fusion = match fusion {
            "vector_only" => Fusion::VectorOnly,
            "text_only" => Fusion::TextOnly,
            "rrf" => Fusion::Rrf { k0: 60 },
            _ => {
                return Err(PyValueError::new_err(
                    "fusion must be one of: vector_only, text_only, rrf",
                ))
            }
        };
        let q = SimilarityQuery {
            context: KnowledgeContext::new(Subject::new(subject)),
            filter: None,
            text,
            vector,
            embedding_model,
            k,
            fusion,
        };
        // P5-M18: the ANN is eventually consistent — a query right after a
        // write must not answer empty. Bounded wait for the maintainer to
        // drain (a broken index is skipped; answers still come, lag is
        // surfaced per hit).
        let healthy = self
            .maintainer
            .status(&self.inner)
            .map(|s| s.status != IndexStatusKind::Error)
            .unwrap_or(true);
        if healthy {
            let _ = self
                .maintainer
                .wait_caught_up(&self.inner, Duration::from_secs(2));
        }
        let hits = py.detach(|| self.inner.find_similar(q).map_err(to_pyerr))?;
        let list = PyList::empty(py);
        for s in hits.iter() {
            list.append(scored_ko_to_py(py, s)).unwrap();
        }
        Ok(list.into_py_any(py).unwrap())
    }

    /// P5-M17b (ND-14): the production declaration surface. Declare a
    /// property index (catalog + registry + synchronous rebuild), settle
    /// the maintainer, then analyze so the CBO can price it. Idempotent —
    /// re-declaring the same shape rebuilds + re-analyzes (the harness
    /// re-declares per cell to refresh M9 stats); a different shape under
    /// the same name fails closed. An index changes plans, never answers.
    #[pyo3(signature = (name, type_name, properties))]
    fn create_index(
        &self,
        py: Python<'_>,
        name: String,
        type_name: String,
        properties: Vec<String>,
    ) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("name", &name).unwrap();
        dict.set_item("type_name", &type_name).unwrap();
        dict.set_item("properties", &properties).unwrap();
        let rows = py.detach(move || {
            let props: Vec<&str> = properties.iter().map(|p| p.as_str()).collect();
            let declared = self.inner.catalog_list_indexes().map_err(to_pyerr)?;
            if let Some(d) = declared.iter().find(|d| d.name == name) {
                if d.type_name != type_name || d.properties != props {
                    return Err(PyValueError::new_err(format!(
                        "index '{name}' already declared with a different shape"
                    )));
                }
                self.inner.rebuild_index(&name).map_err(to_pyerr)?;
            } else {
                self.inner
                    .catalog_create_index(&name, &type_name, &props)
                    .map_err(to_pyerr)?;
            }
            // Settle the maintainer before analyze — a lagging re-apply can
            // transiently overwrite the rebuild with an older version (the
            // M17b wait_caught_up contract).
            self.maintainer
                .wait_caught_up(&self.inner, std::time::Duration::from_secs(300))
                .map_err(to_pyerr)?;
            self.inner
                .analyze(&type_name)
                .map_err(to_pyerr)
                .map(|s| s.row_count)
        })?;
        dict.set_item("rows", rows).unwrap();
        Ok(dict.into_py_any(py).unwrap())
    }

    fn close(&self, _py: Python<'_>) -> PyResult<()> {
        // Kernel holds no explicit close handle in this revision; drop on GC is sufficient.
        Ok(())
    }

    fn relate(
        &self,
        py: Python<'_>,
        subject: &str,
        from_koid: &str,
        to_koid: &str,
        rel_type: &str,
    ) -> PyResult<Py<PyAny>> {
        let from_koid =
            KOID::from_hex(from_koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let to_koid =
            KOID::from_hex(to_koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let rel_type = rel_type.to_string();
        let res = py.detach(move || {
            let subj = Subject::new(&subject);
            let req = RelateRequest::new(subj, from_koid, to_koid, rel_type);
            self.inner.relate(req).map_err(to_pyerr)
        })?;
        let dict = PyDict::new(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py_any(py).unwrap())
    }

    #[pyo3(signature = (subject, koid, rel_type = None, depth = 1))]
    fn traverse(
        &self,
        py: Python<'_>,
        subject: &str,
        koid: &str,
        rel_type: Option<String>,
        depth: usize,
    ) -> PyResult<Py<PyAny>> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let hits = py.detach(move || {
            let subj = Subject::new(&subject);
            let mut q = TraverseQuery::new(subj, koid);
            q.rel_type = rel_type;
            q.depth = depth;
            self.inner.traverse(q).map_err(to_pyerr)
        })?;
        let list = PyList::empty(py);
        for h in hits.iter() {
            let dict = PyDict::new(py);
            dict.set_item("koid", h.koid.to_hex()).unwrap();
            dict.set_item("depth", h.depth).unwrap();
            dict.set_item("rel_type", h.rel_type.clone()).unwrap();
            dict.set_item(
                "direction",
                if h.direction == aikoql_graph::Direction::Outbound {
                    "outbound"
                } else {
                    "inbound"
                },
            )
            .unwrap();
            list.append(dict).unwrap();
        }
        Ok(list.into_py_any(py).unwrap())
    }

    #[pyo3(signature = (query, subject = "query-user"))]
    fn aikoql(&self, py: Python<'_>, query: &str, subject: &str) -> PyResult<Py<PyAny>> {
        let raw = aikoql_compiler::parser::compile_with_subject(query, subject)
            .map_err(PyRuntimeError::new_err)?;
        let plan = aikoql_compiler::planner::Planner::optimize(&raw);
        let result = py.detach(move || {
            aikoql_runtime::Interpreter::execute(&self.inner, &plan).map_err(to_pyerr)
        })?;
        match result {
            aikoql_runtime::RowSet::Objects(kos) => {
                let list = PyList::empty(py);
                for ko in &kos {
                    list.append(ko_to_py(py, ko)).unwrap();
                }
                Ok(list.into_py_any(py).unwrap())
            }
            aikoql_runtime::RowSet::Scored(scored) => {
                let list = PyList::empty(py);
                for (koid, score, type_name, version) in &scored {
                    let dict = PyDict::new(py);
                    dict.set_item("koid", koid.to_hex()).unwrap();
                    dict.set_item("score", *score).unwrap();
                    dict.set_item("type_name", type_name.clone()).unwrap();
                    dict.set_item("version", *version).unwrap();
                    list.append(dict).unwrap();
                }
                Ok(list.into_py_any(py).unwrap())
            }
            _ => Ok(py.None()),
        }
    }
}

#[pymodule]
fn _aikoql(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Aikoql>()?;
    // P3-M9 — version parity with the workspace (sdk001 pins it): the
    // crate version IS the package version (maturin dynamic = ["version"]).
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
