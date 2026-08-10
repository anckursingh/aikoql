//! Mnemosyne Vector Engine — ANN + BM25 index implementations.
//!
//! Provides heavy index implementations behind the kernel's `VectorIndex` and
//! `TextIndex` traits. The traits themselves live in the kernel (like
//! `StorageEngine`); this crate provides pluggable implementations that can be
//! injected into the kernel's `IndexMaintainer`.
//!
//! HLD §5: Vector Engine is a service *around* the kernel, never on the commit
//! path. All indexes here are secondary structures, maintained asynchronously
//! from the Knowledge Event stream.

use mnemosyne_kernel::knowledge::kom::*;
use mnemosyne_kernel::{TextIndex, VectorIndex};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, RwLock};
use tantivy::collector::TopDocs;
use tantivy::query::BooleanQuery;
use tantivy::schema::Value as TantivyValue;
use tantivy::schema::{Field, Schema, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument, Term};

// Re-export the kernel's lightweight impls for convenience.
pub use mnemosyne_kernel::{BruteForceVectorIndex, TokenTextIndex};

// ---------------------------------------------------------------------------
// HnswVectorIndex — approximate nearest-neighbor (fast-hnsw)
// ---------------------------------------------------------------------------

/// HNSW-backed ANN index with model-namespaced partitioning (R7).
/// Labels are `"{model}:{koid_hex}"` so the same KO with different embedding
/// models produces independent HNSW entries.
pub struct HnswVectorIndex {
    dim: usize,
    capacity: usize,
    index: Mutex<hnsw::labeled::LabeledIndex<hnsw::distance::Cosine, String>>,
    /// Track which (KOID, model) pairs are live.
    model_map: RwLock<BTreeMap<(KOID, String), ()>>,
    tombstones: RwLock<BTreeSet<KOID>>,
}

impl HnswVectorIndex {
    pub fn new(dim: usize, capacity: usize) -> Self {
        let index = hnsw::Builder::new()
            .m(16)
            .ef_construction(200)
            .capacity(capacity)
            .seed(42)
            .build_labeled(hnsw::distance::Cosine);
        HnswVectorIndex {
            dim,
            capacity,
            index: Mutex::new(index),
            model_map: RwLock::new(BTreeMap::new()),
            tombstones: RwLock::new(BTreeSet::new()),
        }
    }

    pub fn load(dir: &std::path::Path) -> KResult<Self> {
        let meta_str = std::fs::read_to_string(dir.join("meta.json"))
            .map_err(|e| KError::Store(format!("read hnsw meta: {}", e)))?;
        let meta: serde_json::Value = serde_json::from_str(&meta_str)
            .map_err(|e| KError::Store(format!("parse hnsw meta: {}", e)))?;
        let dim = meta["dim"]
            .as_u64()
            .ok_or_else(|| KError::Store("hnsw meta dim".into()))? as usize;
        let capacity = meta["capacity"]
            .as_u64()
            .ok_or_else(|| KError::Store("hnsw meta capacity".into()))?
            as usize;
        let index =
            hnsw::labeled::LabeledIndex::load(dir.join("index.hnsw"), hnsw::distance::Cosine)
                .map_err(|e| KError::Store(format!("hnsw load: {}", e)))?;
        let tombstones: BTreeSet<KOID> = meta["tombstones"]
            .as_array()
            .ok_or_else(|| KError::Store("hnsw meta tombstones".into()))?
            .iter()
            .filter_map(|v| v.as_str().and_then(|s| KOID::from_hex(s).ok()))
            .collect();
        // R7: models stored as {koid_hex: [model1, model2, ...]}
        let mut model_map: BTreeMap<(KOID, String), ()> = BTreeMap::new();
        if let Some(models_obj) = meta["models"].as_object() {
            for (koid_hex, models_val) in models_obj {
                let koid = KOID::from_hex(koid_hex)
                    .map_err(|e| KError::Store(format!("hnsw meta koid: {}", e)))?;
                if let Some(models_arr) = models_val.as_array() {
                    for m in models_arr {
                        if let Some(model) = m.as_str() {
                            model_map.insert((koid, model.to_string()), ());
                        }
                    }
                } else if let Some(model) = models_val.as_str() {
                    // Backward-compat: old format had single model string.
                    model_map.insert((koid, model.to_string()), ());
                }
            }
        }
        Ok(HnswVectorIndex {
            dim,
            capacity,
            index: Mutex::new(index),
            model_map: RwLock::new(model_map),
            tombstones: RwLock::new(tombstones),
        })
    }
}

impl Default for HnswVectorIndex {
    fn default() -> Self {
        Self::new(128, 10_000)
    }
}

impl VectorIndex for HnswVectorIndex {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]) {
        if vec.len() != self.dim {
            return;
        }
        // R7: label is "{model}:{koid_hex}" so different models produce distinct entries.
        let label = format!("{}:{}", model, koid.to_hex());
        self.index.lock().unwrap().insert(vec.to_vec(), label);
        self.model_map
            .write()
            .unwrap()
            .insert((koid, model.to_string()), ());
        self.tombstones.write().unwrap().remove(&koid);
    }

    fn remove(&self, koid: &KOID) {
        self.tombstones.write().unwrap().insert(*koid);
        self.model_map
            .write()
            .unwrap()
            .retain(|(k, _), _| k != koid);
    }

    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        if qv.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let idx = self.index.lock().unwrap();
        let internal_k = k.saturating_mul(4).min(self.capacity);
        let hits = idx.search(qv, internal_k.max(1), 20);
        let dead = self.tombstones.read().unwrap();
        let models = self.model_map.read().unwrap();
        let mut best: BTreeMap<KOID, f32> = BTreeMap::new();
        for h in hits {
            // R7: label is "{model}:{koid_hex}".
            let (label_model, koid_hex) = match h.payload.split_once(':') {
                Some((m, kh)) => (m, kh),
                None => continue, // skip legacy labels without model prefix
            };
            let koid = KOID::from_hex(koid_hex).unwrap_or(KOID::ZERO);
            if koid == KOID::ZERO || dead.contains(&koid) {
                continue;
            }
            // Model filter.
            if let Some(filter_model) = model {
                if label_model != filter_model {
                    continue;
                }
            }
            // Verify the entry is still tracked in model_map (not partially removed).
            if !models.contains_key(&(koid, label_model.to_string())) {
                continue;
            }
            let sim = 1.0 - h.distance;
            best.entry(koid)
                .and_modify(|s| *s = s.max(sim))
                .or_insert(sim);
        }
        let mut scored: Vec<(KOID, f32)> = best.into_iter().collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        self.model_map.read().unwrap().len()
    }

    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| KError::Store(format!("create hnsw checkpoint dir: {}", e)))?;
        self.index
            .lock()
            .unwrap()
            .save(dir.join("index.hnsw"))
            .map_err(|e| KError::Store(format!("hnsw save: {}", e)))?;
        // R7: models stored as {koid_hex: [model1, model2, ...]}.
        let mut models_json: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for ((koid, model), _) in self.model_map.read().unwrap().iter() {
            models_json
                .entry(koid.to_hex())
                .or_default()
                .push(model.clone());
        }
        let tombstones: Vec<String> = self
            .tombstones
            .read()
            .unwrap()
            .iter()
            .map(|k| k.to_hex())
            .collect();
        let meta = serde_json::json!({
            "dim": self.dim,
            "capacity": self.capacity,
            "tombstones": tombstones,
            "models": models_json,
        });
        std::fs::write(dir.join("meta.json"), meta.to_string())
            .map_err(|e| KError::Store(format!("write hnsw meta: {}", e)))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TantivyTextIndex — BM25 full-text index
// ---------------------------------------------------------------------------

/// Tantivy-backed BM25 index. In-memory by default; `checkpoint` persists to
/// disk. `load` restores from a prior checkpoint.
pub struct TantivyTextIndex {
    koid_field: Field,
    tokens_field: Field,
    index: Index,
    writer: Mutex<IndexWriter>,
    docs: RwLock<BTreeMap<KOID, BTreeSet<String>>>,
}

impl TantivyTextIndex {
    pub fn new() -> Self {
        let mut schema_builder = Schema::builder();
        let koid_field = schema_builder.add_text_field("koid", STRING | STORED);
        let tokens_field = schema_builder.add_text_field("tokens", TEXT | STORED);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let writer = index.writer(15_000_000).expect("tantivy writer");
        TantivyTextIndex {
            koid_field,
            tokens_field,
            index,
            writer: Mutex::new(writer),
            docs: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn load(dir: &std::path::Path) -> KResult<Self> {
        let mut schema_builder = Schema::builder();
        let koid_field = schema_builder.add_text_field("koid", STRING | STORED);
        let tokens_field = schema_builder.add_text_field("tokens", TEXT | STORED);
        let _schema = schema_builder.build();
        let index = Index::open_in_dir(dir)
            .map_err(|e| KError::Store(format!("open tantivy index: {}", e)))?;
        let writer = index
            .writer(15_000_000)
            .map_err(|e| KError::Store(format!("tantivy writer: {}", e)))?;

        let reader = index
            .reader()
            .map_err(|e| KError::Store(format!("tantivy reader: {}", e)))?;
        let searcher = reader.searcher();
        let num_docs = searcher.num_docs();
        let top_docs = searcher
            .search(
                &tantivy::query::AllQuery,
                &TopDocs::with_limit(num_docs as usize),
            )
            .map_err(|e| KError::Store(format!("tantivy scan: {}", e)))?;
        let mut docs: BTreeMap<KOID, BTreeSet<String>> = BTreeMap::new();
        for (_score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| KError::Store(format!("tantivy doc: {}", e)))?;
            let koid_str = doc
                .get_first(koid_field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| KError::Store("tantivy doc koid".into()))?;
            let koid = KOID::from_hex(koid_str)
                .map_err(|e| KError::Store(format!("tantivy koid parse: {}", e)))?;
            let text = doc
                .get_first(tokens_field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| KError::Store("tantivy doc tokens".into()))?;
            let tokens: BTreeSet<String> = text.split_whitespace().map(|s| s.to_string()).collect();
            docs.insert(koid, tokens);
        }
        Ok(TantivyTextIndex {
            koid_field,
            tokens_field,
            index,
            writer: Mutex::new(writer),
            docs: RwLock::new(docs),
        })
    }
}

impl Default for TantivyTextIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl TextIndex for TantivyTextIndex {
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) {
        let key = koid.to_hex();
        let text = tokens.iter().cloned().collect::<Vec<_>>().join(" ");
        let mut w = self.writer.lock().unwrap();
        w.delete_term(Term::from_field_text(self.koid_field, &key));
        w.add_document(doc!(
            self.koid_field => key,
            self.tokens_field => text,
        ))
        .expect("tantivy add_document");
        w.commit().expect("tantivy commit");
        self.docs.write().unwrap().insert(koid, tokens.clone());
    }

    fn remove(&self, koid: &KOID) {
        let key = koid.to_hex();
        let mut w = self.writer.lock().unwrap();
        w.delete_term(Term::from_field_text(self.koid_field, &key));
        w.commit().expect("tantivy commit");
        self.docs.write().unwrap().remove(koid);
    }

    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> Vec<(KOID, f32)> {
        if tokens.is_empty() || k == 0 {
            return Vec::new();
        }
        let terms: Vec<Term> = tokens
            .iter()
            .map(|t| Term::from_field_text(self.tokens_field, t))
            .collect();
        let query = BooleanQuery::new_multiterms_query(terms);
        let reader = self.index.reader().expect("tantivy reader");
        let searcher = reader.searcher();
        let len = self.docs.read().unwrap().len();
        let limit = k.min(len.max(1));
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit))
            .expect("tantivy search");
        top_docs
            .into_iter()
            .filter_map(|(score, doc_address)| {
                let doc: TantivyDocument = searcher.doc(doc_address).ok()?;
                let koid_str = doc.get_first(self.koid_field)?.as_str()?;
                let koid = KOID::from_hex(koid_str).ok()?;
                Some((koid, score))
            })
            .collect()
    }

    fn len(&self) -> usize {
        self.docs.read().unwrap().len()
    }

    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| KError::Store(format!("create tantivy checkpoint dir: {}", e)))?;
        let mut schema_builder = Schema::builder();
        let koid_field = schema_builder.add_text_field("koid", STRING | STORED);
        let tokens_field = schema_builder.add_text_field("tokens", TEXT | STORED);
        let schema = schema_builder.build();
        let disk_index = Index::create_in_dir(dir, schema)
            .map_err(|e| KError::Store(format!("create tantivy disk index: {}", e)))?;
        let mut w = disk_index
            .writer(15_000_000)
            .map_err(|e| KError::Store(format!("tantivy disk writer: {}", e)))?;
        for (koid, tokens) in self.docs.read().unwrap().iter() {
            let text = tokens.iter().cloned().collect::<Vec<_>>().join(" ");
            w.add_document(doc!(
                koid_field => koid.to_hex(),
                tokens_field => text,
            ))
            .map_err(|e| KError::Store(format!("tantivy checkpoint add: {}", e)))?;
        }
        w.commit()
            .map_err(|e| KError::Store(format!("tantivy checkpoint commit: {}", e)))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kid(n: u8) -> KOID {
        KOID([n; KOID_LEN])
    }

    #[test]
    fn hnsw_vector_orders_and_removes() {
        let idx = HnswVectorIndex::new(2, 100);
        let a = kid(1);
        let b = kid(2);
        let c = kid(3);
        idx.upsert(a, "m", &[1.0, 0.0]);
        idx.upsert(b, "m", &[0.9, 0.1]);
        idx.upsert(c, "n", &[0.0, 1.0]); // different model
        let r = idx.search(&[1.0, 0.0], 2, None);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, a);
        assert_eq!(r[1].0, b);
        // Model filter: only model "m"
        let filtered = idx.search(&[1.0, 0.0], 5, Some("m"));
        assert_eq!(filtered.len(), 2);
        // Model filter: only model "n"
        let n_only = idx.search(&[0.0, 1.0], 5, Some("n"));
        assert_eq!(n_only.len(), 1);
        assert_eq!(n_only[0].0, c);
        idx.remove(&a);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.search(&[1.0, 0.0], 1, None)[0].0, b);
    }

    #[test]
    fn hnsw_model_namespaced_partitioning() {
        // R7: same KOID with different models → independent HNSW entries.
        let idx = HnswVectorIndex::new(2, 100);
        let a = kid(1);
        let b = kid(2);
        idx.upsert(a, "bge-m3", &[1.0, 0.0]);
        idx.upsert(a, "text-embed-3", &[0.0, 1.0]);
        idx.upsert(b, "bge-m3", &[0.9, 0.1]);
        assert_eq!(idx.len(), 3); // three (koid, model) pairs
                                  // Model filter: only bge-m3.
        let bge = idx.search(&[1.0, 0.0], 10, Some("bge-m3"));
        assert_eq!(bge.len(), 2); // a+b both in bge-m3
                                  // Model filter: only text-embed-3.
        let te3 = idx.search(&[0.0, 1.0], 10, Some("text-embed-3"));
        assert_eq!(te3.len(), 1);
        assert_eq!(te3[0].0, a);
        // Remove a: all model entries for a are gone.
        idx.remove(&a);
        assert_eq!(idx.len(), 1); // only (b, bge-m3) left
        assert!(idx
            .search(&[1.0, 0.0], 10, Some("bge-m3"))
            .iter()
            .all(|(k, _)| *k == b));
    }

    #[test]
    fn tantivy_text_index_orders_and_removes() {
        let idx = TantivyTextIndex::new();
        let a = kid(1);
        let b = kid(2);
        idx.upsert(a, &BTreeSet::from(["cats".to_string(), "dogs".to_string()]));
        idx.upsert(b, &BTreeSet::from(["birds".to_string()]));
        let r = idx.search(&BTreeSet::from(["cats".to_string()]), 5);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, a);
        idx.remove(&a);
        assert!(idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .is_empty());
    }
}
