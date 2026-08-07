//! Python SDK for the Mnemosyne Knowledge Kernel.
//!
//! Exposes a minimal, synchronous `Mnemosyne` class backed by the durable
//! kernel. The LangGraph checkpointer wrapper lives in pure Python on top of
//! these primitives (`mnemosyne.checkpointer`).

use mnemosyne_graph::{GraphEngineApi, RelateRequest, TraverseQuery};
use mnemosyne_kernel::{
    Fusion, Kernel, KnowledgeContext, Metadata, RedbEngine, RememberRequest, ScoredKO,
    SemanticBlock, SimilarityQuery, Subject, SystemClock, Value, KOID,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::PyObject;
use std::collections::BTreeMap;
use std::sync::Arc;

fn to_pyerr(e: mnemosyne_kernel::KError) -> PyErr {
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
    } else if let Ok(list) = obj.clone().downcast::<PyList>() {
        let mut v = Vec::new();
        for item in list.iter() {
            v.push(value_from_py(&item)?);
        }
        Ok(Value::List(v))
    } else if let Ok(dict) = obj.clone().downcast::<PyDict>() {
        let mut m = BTreeMap::new();
        for (k, val) in dict.iter() {
            let key: String = k.extract()?;
            m.insert(key, value_from_py(&val)?);
        }
        Ok(Value::Map(m))
    } else {
        Err(PyValueError::new_err(
            "unsupported Python value type for Mnemosyne property",
        ))
    }
}

fn value_to_py(py: Python<'_>, v: &Value) -> PyObject {
    match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py(py),
        Value::Int(i) => i.into_py(py),
        Value::Float(f) => f.into_py(py),
        Value::Text(s) => s.clone().into_py(py),
        Value::Bytes(b) => b.clone().into_py(py),
        Value::List(items) => {
            let list = PyList::empty_bound(py);
            for item in items {
                list.append(value_to_py(py, item)).unwrap();
            }
            list.into_py(py)
        }
        Value::Map(m) => {
            let dict = PyDict::new_bound(py);
            for (k, val) in m.iter() {
                dict.set_item(k, value_to_py(py, val)).unwrap();
            }
            dict.into_py(py)
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

fn ko_to_py(py: Python<'_>, ko: &mnemosyne_kernel::KnowledgeObject) -> PyObject {
    let dict = PyDict::new_bound(py);
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
    dict.into_py(py)
}

fn props_to_py(py: Python<'_>, props: &BTreeMap<String, Value>) -> PyObject {
    let dict = PyDict::new_bound(py);
    for (k, v) in props.iter() {
        dict.set_item(k, value_to_py(py, v)).unwrap();
    }
    dict.into_py(py)
}

fn scored_ko_to_py(py: Python<'_>, s: &ScoredKO) -> PyObject {
    let dict = PyDict::new_bound(py);
    dict.set_item("ko", ko_to_py(py, &s.ko)).unwrap();
    dict.set_item("score", s.score).unwrap();
    dict.set_item("index_lag_ms", s.index_lag_ms).unwrap();
    dict.into_py(py)
}

#[pyclass]
pub struct Mnemosyne {
    inner: Arc<Kernel>,
}

#[pymethods]
impl Mnemosyne {
    #[new]
    #[pyo3(signature = (path, salt = 0))]
    fn new(path: &str, salt: u64) -> PyResult<Self> {
        let engine = RedbEngine::open(path).map_err(to_pyerr)?;
        let kernel =
            Kernel::open(Arc::new(engine), Arc::new(SystemClock), salt).map_err(to_pyerr)?;
        Ok(Mnemosyne {
            inner: Arc::new(kernel),
        })
    }

    #[pyo3(signature = (subject, type_name, properties, semantic = None, roles = None))]
    fn remember(
        &self,
        py: Python<'_>,
        subject: &str,
        type_name: &str,
        properties: &Bound<'_, PyDict>,
        semantic: Option<&Bound<'_, PyDict>>,
        roles: Option<Vec<String>>,
    ) -> PyResult<PyObject> {
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

        let res = py.allow_threads(move || {
            let subject = Subject::with_roles(
                subject,
                &roles.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            );
            let mut req = RememberRequest::create(
                subject,
                Metadata {
                    type_name,
                    tenant: None,
                    schema_version: 1,
                    tags: vec![],
                },
            );
            req.properties = prop_map;
            req.semantic = semantic;
            self.inner.remember(req).map_err(to_pyerr)
        })?;
        let dict = PyDict::new_bound(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py(py))
    }

    fn get(&self, py: Python<'_>, subject: &str, koid: &str) -> PyResult<PyObject> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let ko = py.allow_threads(move || {
            let subj = Subject::new(&subject);
            self.inner.get(&subj, &koid).map_err(to_pyerr)
        })?;
        Ok(ko_to_py(py, &ko))
    }

    fn forget(&self, py: Python<'_>, subject: &str, koid: &str) -> PyResult<PyObject> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let res = py.allow_threads(move || {
            let subj = Subject::new(&subject);
            self.inner
                .forget(
                    &subj,
                    &koid,
                    mnemosyne_kernel::ForgetMode::Tombstone,
                    None,
                    None,
                )
                .map_err(to_pyerr)
        })?;
        let dict = PyDict::new_bound(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py(py))
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
    ) -> PyResult<PyObject> {
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
        let hits = py.allow_threads(|| self.inner.find_similar(q).map_err(to_pyerr))?;
        let list = PyList::empty_bound(py);
        for s in hits.iter() {
            list.append(scored_ko_to_py(py, s)).unwrap();
        }
        Ok(list.into_py(py))
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
    ) -> PyResult<PyObject> {
        let from_koid =
            KOID::from_hex(from_koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let to_koid =
            KOID::from_hex(to_koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let rel_type = rel_type.to_string();
        let res = py.allow_threads(move || {
            let subj = Subject::new(&subject);
            let req = RelateRequest::new(subj, from_koid, to_koid, rel_type);
            self.inner.relate(req).map_err(to_pyerr)
        })?;
        let dict = PyDict::new_bound(py);
        dict.set_item("koid", res.koid.to_hex()).unwrap();
        dict.set_item("version", res.version).unwrap();
        dict.set_item("commit_ts", res.commit_ts).unwrap();
        Ok(dict.into_py(py))
    }

    #[pyo3(signature = (subject, koid, rel_type = None, depth = 1))]
    fn traverse(
        &self,
        py: Python<'_>,
        subject: &str,
        koid: &str,
        rel_type: Option<String>,
        depth: usize,
    ) -> PyResult<PyObject> {
        let koid = KOID::from_hex(koid).map_err(|e| PyValueError::new_err(format!("{}", e)))?;
        let subject = subject.to_string();
        let hits = py.allow_threads(move || {
            let subj = Subject::new(&subject);
            let mut q = TraverseQuery::new(subj, koid);
            q.rel_type = rel_type;
            q.depth = depth;
            self.inner.traverse(q).map_err(to_pyerr)
        })?;
        let list = PyList::empty_bound(py);
        for h in hits.iter() {
            let dict = PyDict::new_bound(py);
            dict.set_item("koid", h.koid.to_hex()).unwrap();
            dict.set_item("depth", h.depth).unwrap();
            dict.set_item("rel_type", h.rel_type.clone()).unwrap();
            dict.set_item(
                "direction",
                if h.direction == mnemosyne_graph::Direction::Outbound {
                    "outbound"
                } else {
                    "inbound"
                },
            )
            .unwrap();
            list.append(dict).unwrap();
        }
        Ok(list.into_py(py))
    }

    #[pyo3(signature = (query, subject = "query-user"))]
    fn aikoql(&self, py: Python<'_>, query: &str, subject: &str) -> PyResult<PyObject> {
        let raw = mnemosyne_compiler::parser::compile_with_subject(query, subject)
            .map_err(|e| PyRuntimeError::new_err(e))?;
        let plan = mnemosyne_compiler::planner::Planner::optimize(&raw);
        let result = py.allow_threads(move || {
            mnemosyne_runtime::Interpreter::execute(&self.inner, &plan).map_err(to_pyerr)
        })?;
        match result {
            mnemosyne_runtime::RowSet::Objects(kos) => {
                let list = PyList::empty_bound(py);
                for ko in &kos {
                    list.append(ko_to_py(py, ko)).unwrap();
                }
                Ok(list.into_py(py))
            }
            mnemosyne_runtime::RowSet::Scored(scored) => {
                let list = PyList::empty_bound(py);
                for (koid, score, type_name, version) in &scored {
                    let dict = PyDict::new_bound(py);
                    dict.set_item("koid", koid.to_hex()).unwrap();
                    dict.set_item("score", *score).unwrap();
                    dict.set_item("type_name", type_name.clone()).unwrap();
                    dict.set_item("version", *version).unwrap();
                    list.append(dict).unwrap();
                }
                Ok(list.into_py(py))
            }
            _ => Ok(py.None()),
        }
    }
}

#[pymodule]
fn _mnemosyne(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Mnemosyne>()?;
    Ok(())
}
