//! Aikoql Vector Engine — ANN + BM25 index implementations.
//!
//! Provides heavy index implementations behind the kernel's `VectorIndex` and
//! `TextIndex` traits. The traits themselves live in the kernel (like
//! `StorageEngine`); this crate provides pluggable implementations that can be
//! injected into the kernel's `IndexMaintainer`.
//!
//! HLD §5: Vector Engine is a service *around* the kernel, never on the commit
//! path. All indexes here are secondary structures, maintained asynchronously
//! from the Knowledge Event stream.

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::{TextIndex, VectorHealth, VectorIndex};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};
use tantivy::collector::TopDocs;
use tantivy::query::BooleanQuery;
use tantivy::schema::Value as TantivyValue;
use tantivy::schema::{Field, Schema, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument, Term};

// Re-export the kernel's lightweight impls for convenience.
pub use aikoql_kernel::{BruteForceVectorIndex, TokenTextIndex};

// ---------------------------------------------------------------------------
// HnswVectorIndex — approximate nearest-neighbor (fast-hnsw)
// ---------------------------------------------------------------------------

/// P4-M7 (TDD-VECTOR-002): a delete tombstones (the graph keeps the node);
/// once dead nodes pass this ratio of physical nodes, the next maintenance
/// tick rebuilds the graph from the live set — ONCE, never per delete.
const REBUILD_DEAD_RATIO: f64 = 0.3;

/// HNSW-backed ANN index with model-namespaced partitioning (R7).
/// Labels are `"{model}:{koid_hex}"` so the same KO with different embedding
/// models produces independent HNSW entries.
pub struct HnswVectorIndex {
    /// 0 = adopt the first upsert's dim (P5-M18 vec003: enrichers and fixtures
    /// disagree on dim, a fixed default would silently drop one).
    dim: AtomicUsize,
    capacity: usize,
    index: Mutex<hnsw::labeled::LabeledIndex<hnsw::distance::Cosine, String>>,
    /// Live (KOID, model) pairs and their vectors (kept so a rebuild can
    /// re-insert them; empty vectors mean a post-load entry not yet
    /// re-upserted by the maintainer).
    model_map: RwLock<BTreeMap<(KOID, String), Vec<f32>>>,
    tombstones: RwLock<BTreeSet<KOID>>,
    /// Nodes physically in the graph (inserts; removes never shrink it).
    physical: AtomicU64,
    /// P5-M27 (IDX-P1-03): upserts dropped on a dimension mismatch (the
    /// adopted dim is global — a second embedding model with a different
    /// dim would drop every vector silently without this signal).
    dropped_dim_mismatch: AtomicU64,
    pending_rebuild: AtomicBool,
    /// P5-M22 (P1-14): the checkpoint generation. `checkpoint` bumps it and
    /// publishes `manifest.json` last; `load` gates on manifest == meta —
    /// a pair torn across two checkpoints fails closed.
    gen: AtomicU64,
}

fn build_index(capacity: usize) -> hnsw::labeled::LabeledIndex<hnsw::distance::Cosine, String> {
    hnsw::Builder::new()
        .m(16)
        .ef_construction(200)
        .capacity(capacity)
        .seed(42)
        .build_labeled(hnsw::distance::Cosine)
}

impl HnswVectorIndex {
    pub fn new(dim: usize, capacity: usize) -> Self {
        HnswVectorIndex {
            dim: AtomicUsize::new(dim),
            capacity,
            index: Mutex::new(build_index(capacity)),
            model_map: RwLock::new(BTreeMap::new()),
            tombstones: RwLock::new(BTreeSet::new()),
            physical: AtomicU64::new(0),
            dropped_dim_mismatch: AtomicU64::new(0),
            pending_rebuild: AtomicBool::new(false),
            gen: AtomicU64::new(0),
        }
    }

    pub fn load(dir: &std::path::Path) -> KResult<Self> {
        // P5-M22 (P1-14): the manifest is the gate — published atomically
        // last by `checkpoint`, so its presence certifies a complete pair.
        // A pair without one (or with mismatched generations) fails closed.
        let manifest_str = std::fs::read_to_string(dir.join("manifest.json"))
            .map_err(|e| KError::Store(format!("read hnsw manifest: {}", e)))?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest_str)
            .map_err(|e| KError::Store(format!("parse hnsw manifest: {}", e)))?;
        let manifest_gen = manifest["generation"]
            .as_u64()
            .ok_or_else(|| KError::Store("hnsw manifest generation".into()))?;
        let meta_str = std::fs::read_to_string(dir.join("meta.json"))
            .map_err(|e| KError::Store(format!("read hnsw meta: {}", e)))?;
        let meta: serde_json::Value = serde_json::from_str(&meta_str)
            .map_err(|e| KError::Store(format!("parse hnsw meta: {}", e)))?;
        let meta_gen = meta["generation"]
            .as_u64()
            .ok_or_else(|| KError::Store("hnsw meta generation".into()))?;
        if meta_gen != manifest_gen {
            return Err(KError::Store(format!(
                "hnsw checkpoint torn: meta generation {meta_gen} != manifest {manifest_gen}"
            )));
        }
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
        let mut model_map: BTreeMap<(KOID, String), Vec<f32>> = BTreeMap::new();
        if let Some(models_obj) = meta["models"].as_object() {
            for (koid_hex, models_val) in models_obj {
                let koid = KOID::from_hex(koid_hex)
                    .map_err(|e| KError::Store(format!("hnsw meta koid: {}", e)))?;
                if let Some(models_arr) = models_val.as_array() {
                    for m in models_arr {
                        if let Some(model) = m.as_str() {
                            model_map.insert((koid, model.to_string()), Vec::new());
                        }
                    }
                } else if let Some(model) = models_val.as_str() {
                    // Backward-compat: old format had single model string.
                    model_map.insert((koid, model.to_string()), Vec::new());
                }
            }
        }
        let physical = meta["physical"].as_u64().unwrap_or(model_map.len() as u64);
        Ok(HnswVectorIndex {
            dim: AtomicUsize::new(dim),
            capacity,
            index: Mutex::new(index),
            model_map: RwLock::new(model_map),
            tombstones: RwLock::new(tombstones),
            physical: AtomicU64::new(physical),
            // P5-M27 (IDX-P1-03): an ephemeral diagnostic — restarts the
            // count, it is not part of the checkpointed generation.
            dropped_dim_mismatch: AtomicU64::new(0),
            // P5-M27 (IDX-P1-04): restore the armed trigger (false for
            // pre-M27 checkpoints — their dead ratio still crosses, but no
            // flag means no rebuild until the next delete re-arms it).
            pending_rebuild: AtomicBool::new(meta["pending"].as_bool().unwrap_or(false)),
            gen: AtomicU64::new(meta_gen),
        })
    }

    fn dim(&self) -> usize {
        self.dim.load(Ordering::Relaxed)
    }

    fn dead_ratio(&self) -> f64 {
        let physical = self.physical.load(Ordering::Relaxed) as f64;
        if physical == 0.0 {
            return 0.0;
        }
        // justified: RwLock poison is unrecoverable
        self.tombstones.read().unwrap().len() as f64 / physical
    }

    /// P4-M7 (TDD-VECTOR-002): rebuild ONCE when a past delete crossed the
    /// dead ratio (a delete itself only flags — it stays O(1)). Re-upserts
    /// that heal the ratio below the threshold cancel the pending rebuild.
    pub fn maybe_rebuild(&self) -> bool {
        if !self.pending_rebuild.swap(false, Ordering::Relaxed) {
            return false;
        }
        if self.dead_ratio() <= REBUILD_DEAD_RATIO {
            return false;
        }
        // justified: RwLock poison is unrecoverable
        let live: Vec<(String, Vec<f32>)> = self
            .model_map
            .read()
            .unwrap()
            .iter()
            .map(|((koid, model), v)| (format!("{model}:{}", koid.to_hex()), v.clone()))
            .collect();
        if live.iter().any(|(_, v)| v.is_empty()) {
            // P5-M27 (IDX-P1-04): post-load state — checkpoints store labels,
            // not vectors, and empty vectors are not a rebuild. Keep the
            // trigger armed and retry on a later tick (the catch-up re-upserts
            // refill the map). Clearing it here would lose the trigger
            // forever — no later delete crosses the ratio again.
            self.pending_rebuild.store(true, Ordering::Relaxed);
            return false;
        }
        let mut fresh = build_index(self.capacity);
        for (label, vec) in &live {
            fresh.insert(vec.clone(), label.clone());
        }
        // justified: Mutex poison is unrecoverable
        *self.index.lock().unwrap() = fresh;
        self.physical.store(live.len() as u64, Ordering::Relaxed);
        // justified: RwLock poison is unrecoverable
        self.tombstones.write().unwrap().clear();
        true
    }
}

impl Default for HnswVectorIndex {
    fn default() -> Self {
        Self::new(128, 10_000)
    }
}

/// P5-M27 (IDX-P0-02) test hook — mirror of the scheduler's checkpoint_park
/// with the same env contract: CHECKPOINT_PARK_AT (stage name),
/// CHECKPOINT_PARK_ACK (append "parked {stage}"), CHECKPOINT_PARK_RELEASE
/// (file whose existence releases the park). Freezes the HNSW checkpoint
/// between meta.json and manifest.json — the torn-pair window.
fn checkpoint_park(stage: &str) {
    if std::env::var_os("CHECKPOINT_PARK_AT")
        .map(|s| s == stage)
        .unwrap_or(false)
    {
        if let Some(ack) = std::env::var_os("CHECKPOINT_PARK_ACK") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&ack)
            {
                let _ = writeln!(f, "parked {stage}");
            }
        }
        let release = std::env::var_os("CHECKPOINT_PARK_RELEASE")
            .expect("CHECKPOINT_PARK_RELEASE required with CHECKPOINT_PARK_AT");
        let mut waited = 0u64;
        while !std::path::Path::new(&release).exists() && waited < 60_000 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += 10;
        }
    }
}

impl VectorIndex for HnswVectorIndex {
    fn upsert(&self, koid: KOID, model: &str, vec: &[f32]) {
        // P5-M18 (vec003): dim 0 adopts the first vector's dim.
        let dim = self.dim();
        if dim == 0 {
            self.dim.store(vec.len(), Ordering::Relaxed);
        } else if vec.len() != dim {
            // P5-M27 (IDX-P1-03): count the drop — a mismatch is a
            // data-quality event, never silently "healthy" index state.
            self.dropped_dim_mismatch.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // R7: label is "{model}:{koid_hex}" so different models produce distinct entries.
        let label = format!("{}:{}", model, koid.to_hex());
        // justified: Mutex poison is unrecoverable
        self.index.lock().unwrap().insert(vec.to_vec(), label);
        // P5-M23 (P1-17): physical counts DISTINCT (koid, model) entries —
        // a re-upsert updates the vector, it does not add a node (the
        // maintainer re-upserts every replay pass, so inflation would hide
        // real dead nodes behind dead_ratio = tombstones / physical). The
        // stale graph node is deduped by the next rebuild.
        // justified: RwLock poison is unrecoverable
        let mut mm = self.model_map.write().unwrap();
        if mm.insert((koid, model.to_string()), vec.to_vec()).is_none() {
            self.physical.fetch_add(1, Ordering::Relaxed);
        }
        drop(mm);
        // justified: RwLock poison is unrecoverable
        self.tombstones.write().unwrap().remove(&koid);
    }

    fn remove(&self, koid: &KOID) {
        // justified: RwLock poison is unrecoverable
        self.tombstones.write().unwrap().insert(*koid);
        self.model_map
            .write()
            // justified: RwLock poison is unrecoverable
            .unwrap()
            .retain(|(k, _), _| k != koid);
        // P4-M7 (TDD-VECTOR-002): flag a rebuild when the dead ratio crosses
        // the threshold — the delete itself stays O(1).
        if self.dead_ratio() > REBUILD_DEAD_RATIO {
            self.pending_rebuild.store(true, Ordering::Relaxed);
        }
    }

    fn search(&self, qv: &[f32], k: usize, model: Option<&str>) -> Vec<(KOID, f32)> {
        let dim = self.dim();
        if dim == 0 || qv.len() != dim || k == 0 {
            return Vec::new();
        }
        // justified: Mutex poison is unrecoverable
        let idx = self.index.lock().unwrap();
        // P5-M23 (P0-10): capacity is an allocator hint — the graph grows
        // past it. Bound the candidate pool by the NODES THAT EXIST, not
        // the initial hint: k*4 keeps the recall margin, and the
        // coordinator's usize::MAX (ann001) still lands on a finite,
        // node-count-sized pool (fast-hnsw does ef = ef.max(k), so an
        // unbounded k would allocate an unbounded heap).
        let internal_k = k.saturating_mul(4).min(
            self.physical
                .load(Ordering::Relaxed)
                .max(self.capacity as u64) as usize,
        );
        let hits = idx.search(qv, internal_k.max(1), 20);
        // justified: RwLock poison is unrecoverable
        let dead = self.tombstones.read().unwrap();
        // justified: RwLock poison is unrecoverable
        let models = self.model_map.read().unwrap();
        let mut best: BTreeMap<KOID, f32> = BTreeMap::new();
        for h in hits {
            // R7: label is "{model}:{koid_hex}".
            let (label_model, koid_hex) = match h.payload.split_once(':') {
                Some((m, kh)) => (m, kh),
                None => continue, // skip legacy labels without model prefix
            };
            // justified: legacy/malformed label → KOID::ZERO, skipped below
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
                // justified: NaN (zero-vector cosine) ties deterministically
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
        self.model_map.read().unwrap().len()
    }

    fn health(&self) -> Option<VectorHealth> {
        Some(VectorHealth {
            live: self.model_map.read().unwrap().len(),
            physical: self.physical.load(Ordering::Relaxed) as usize,
            tombstones: self.tombstones.read().unwrap().len(),
            dead_ratio: self.dead_ratio(),
            // P5-M23 (P0-10): health reports real capacity/usage.
            dim: self.dim(),
            capacity: self.capacity,
            dropped_dim_mismatch: self.dropped_dim_mismatch.load(Ordering::Relaxed),
        })
    }

    fn maybe_rebuild(&self) -> bool {
        HnswVectorIndex::maybe_rebuild(self)
    }

    fn checkpoint(&self, dir: &std::path::Path) -> KResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| KError::Store(format!("create hnsw checkpoint dir: {}", e)))?;
        // P5-M22 (P1-14): every checkpoint is a new generation.
        let generation = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.index
            .lock()
            // justified: Mutex poison is unrecoverable
            .unwrap()
            .save(dir.join("index.hnsw"))
            .map_err(|e| KError::Store(format!("hnsw save: {}", e)))?;
        // R7: models stored as {koid_hex: [model1, model2, ...]}.
        let mut models_json: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // justified: RwLock poison is unrecoverable
        for (koid, model) in self.model_map.read().unwrap().keys() {
            models_json
                .entry(koid.to_hex())
                .or_default()
                .push(model.clone());
        }
        let tombstones: Vec<String> = self
            .tombstones
            .read()
            // justified: RwLock poison is unrecoverable
            .unwrap()
            .iter()
            .map(|k| k.to_hex())
            .collect();
        let meta = serde_json::json!({
            "dim": self.dim(),
            "capacity": self.capacity,
            "physical": self.physical.load(Ordering::Relaxed),
            "tombstones": tombstones,
            // P5-M27 (IDX-P1-04): the armed rebuild trigger is part of the
            // generation — a crash between the delete and the rebuild must
            // not disarm it (the dead ratio stays crossed post-load).
            "pending": self.pending_rebuild.load(Ordering::Relaxed),
            "models": models_json,
            // P5-M22 (P1-14): the generation binds the meta to the graph
            // below; the manifest (published LAST) is the load gate.
            "generation": generation,
        });
        std::fs::write(dir.join("meta.json"), meta.to_string())
            .map_err(|e| KError::Store(format!("write hnsw meta: {}", e)))?;
        // P5-M27 (IDX-P0-02): the torn-pair window — meta written, the
        // manifest (the load gate) not yet. Killed here, the generation is
        // never loadable, and the funnel's COMPLETE gate keeps the half
        // pair from publication; the crash matrix parks this window.
        checkpoint_park("hnsw-manifest");
        // Published atomically last — its presence certifies the pair.
        let manifest = serde_json::json!({ "generation": generation });
        std::fs::write(dir.join("manifest.json"), manifest.to_string())
            .map_err(|e| KError::Store(format!("write hnsw manifest: {}", e)))?;
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
    /// R4: returns KResult — a failed tantivy writer is unrecoverable at
    /// this layer, so it propagates to the caller.
    pub fn new() -> KResult<Self> {
        let mut schema_builder = Schema::builder();
        let koid_field = schema_builder.add_text_field("koid", STRING | STORED);
        let tokens_field = schema_builder.add_text_field("tokens", TEXT | STORED);
        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let writer = index
            .writer(15_000_000)
            .map_err(|e| KError::Store(format!("tantivy writer: {}", e)))?;
        Ok(TantivyTextIndex {
            koid_field,
            tokens_field,
            index,
            writer: Mutex::new(writer),
            docs: RwLock::new(BTreeMap::new()),
        })
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
        // An empty checkpoint is legal (the maintainer can checkpoint
        // before the first note seeds tokens): TopDocs::with_limit(0)
        // panics inside tantivy, so short-circuit the scan.
        if num_docs == 0 {
            return Ok(TantivyTextIndex {
                koid_field,
                tokens_field,
                index,
                writer: Mutex::new(writer),
                docs: RwLock::new(BTreeMap::new()),
            });
        }
        let top_docs = searcher
            .search(
                &tantivy::query::AllQuery,
                &TopDocs::with_limit(num_docs as usize).order_by_score(),
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
        // justified: Default cannot return a Result; a writer failure here is
        // unrecoverable for an in-RAM index
        Self::new().expect("tantivy writer")
    }
}

impl TextIndex for TantivyTextIndex {
    fn upsert(&self, koid: KOID, tokens: &BTreeSet<String>) -> KResult<()> {
        let key = koid.to_hex();
        let text = tokens.iter().cloned().collect::<Vec<_>>().join(" ");
        // justified: Mutex poison is unrecoverable
        let mut w = self.writer.lock().unwrap();
        w.delete_term(Term::from_field_text(self.koid_field, &key));
        w.add_document(doc!(
            self.koid_field => key,
            self.tokens_field => text,
        ))
        .map_err(|e| KError::Store(format!("tantivy add_document: {}", e)))?;
        w.commit()
            .map_err(|e| KError::Store(format!("tantivy commit: {}", e)))?;
        // justified: RwLock poison is unrecoverable
        self.docs.write().unwrap().insert(koid, tokens.clone());
        Ok(())
    }

    /// P4-M7 (TDD-VECTOR-001): one delete pass + one add pass + ONE commit
    /// for the whole batch. A repeated koid keeps its LAST write (one doc).
    fn upsert_many(&self, items: &[(KOID, BTreeSet<String>)]) -> KResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        let unique: BTreeMap<KOID, String> = items
            .iter()
            .map(|(k, t)| (*k, t.iter().cloned().collect::<Vec<_>>().join(" ")))
            .collect();
        // justified: Mutex poison is unrecoverable
        let mut w = self.writer.lock().unwrap();
        for koid in unique.keys() {
            w.delete_term(Term::from_field_text(self.koid_field, &koid.to_hex()));
        }
        for (koid, text) in &unique {
            w.add_document(doc!(
                self.koid_field => koid.to_hex(),
                self.tokens_field => text.as_str(),
            ))
            .map_err(|e| KError::Store(format!("tantivy batch add_document: {}", e)))?;
        }
        w.commit()
            .map_err(|e| KError::Store(format!("tantivy batch commit: {}", e)))?;
        // justified: RwLock poison is unrecoverable
        self.docs.write().unwrap().extend(
            unique
                .iter()
                .map(|(k, t)| (*k, t.split_whitespace().map(String::from).collect())),
        );
        Ok(())
    }

    fn remove_many(&self, koids: &[KOID]) -> KResult<()> {
        if koids.is_empty() {
            return Ok(());
        }
        // justified: Mutex poison is unrecoverable
        let mut w = self.writer.lock().unwrap();
        for koid in koids {
            w.delete_term(Term::from_field_text(self.koid_field, &koid.to_hex()));
        }
        w.commit()
            .map_err(|e| KError::Store(format!("tantivy batch commit: {}", e)))?;
        // justified: RwLock poison is unrecoverable
        self.docs.write().unwrap().retain(|k, _| !koids.contains(k));
        Ok(())
    }

    fn remove(&self, koid: &KOID) -> KResult<()> {
        let key = koid.to_hex();
        // justified: Mutex poison is unrecoverable
        let mut w = self.writer.lock().unwrap();
        w.delete_term(Term::from_field_text(self.koid_field, &key));
        w.commit()
            .map_err(|e| KError::Store(format!("tantivy commit: {}", e)))?;
        // justified: RwLock poison is unrecoverable
        self.docs.write().unwrap().remove(koid);
        Ok(())
    }

    fn search(&self, tokens: &BTreeSet<String>, k: usize) -> KResult<Vec<(KOID, f32)>> {
        if tokens.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let terms: Vec<Term> = tokens
            .iter()
            .map(|t| Term::from_field_text(self.tokens_field, t))
            .collect();
        let query = BooleanQuery::new_multiterms_query(terms);
        let reader = self
            .index
            .reader()
            .map_err(|e| KError::Store(format!("tantivy reader: {}", e)))?;
        let searcher = reader.searcher();
        // justified: RwLock poison is unrecoverable
        let len = self.docs.read().unwrap().len();
        let limit = k.min(len.max(1));
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| KError::Store(format!("tantivy search: {}", e)))?;
        Ok(top_docs
            .into_iter()
            .filter_map(|(score, doc_address)| {
                let doc: TantivyDocument = searcher.doc(doc_address).ok()?;
                let koid_str = doc.get_first(self.koid_field)?.as_str()?;
                let koid = KOID::from_hex(koid_str).ok()?;
                Some((koid, score))
            })
            .collect())
    }

    fn len(&self) -> usize {
        // justified: RwLock poison is unrecoverable
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
        // justified: RwLock poison is unrecoverable
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

    // --- P5-M18 — vec003: data-driven dim (0 = adopt the first vector's).
    // RED: HnswVectorIndex::new(0, _) still hard-fixes dim 0 — every upsert
    // is silently dropped, every query empty. MCP enrichers emit 384-d,
    // harness fixtures 2-d: a fixed default dim would silently drop one. ---

    #[test]
    fn vec003_zero_dim_adopts_first_upsert() {
        let idx = HnswVectorIndex::new(0, 100);
        let a = kid(1);
        let b = kid(2);
        idx.upsert(a, "m", &[1.0, 0.0, 0.5]); // 3-d wins
        idx.upsert(b, "m", &[0.9, 0.1, 0.2]); // same dim — accepted
        idx.upsert(kid(3), "m", &[0.5, 0.5]); // 2-d — dropped
        assert_eq!(idx.len(), 2, "mismatched-dim upsert is dropped");
        assert!(
            idx.search(&[1.0, 0.0], 5, None).is_empty(),
            "wrong-dim query answers empty"
        );
        let r = idx.search(&[1.0, 0.0, 0.5], 5, None);
        assert_eq!(r.len(), 2, "adopted-dim query works");
        assert_eq!(r[0].0, a);
    }

    #[test]
    fn vec003_checkpoint_load_roundtrips_adopted_dim() {
        // justified: test-thread names contain `::` (invalid on Windows paths)
        let dir = std::env::temp_dir().join(format!("aikoql-vec003-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let idx = HnswVectorIndex::new(0, 100);
            idx.upsert(kid(1), "m", &[1.0, 0.0, 0.5]);
            idx.checkpoint(&dir).unwrap();
        }
        let loaded = HnswVectorIndex::load(&dir).unwrap();
        loaded.upsert(kid(2), "m", &[0.9, 0.1, 0.2]); // adopted dim survives load
        loaded.upsert(kid(3), "m", &[0.5, 0.5]); // wrong dim — dropped
        assert_eq!(loaded.len(), 2);
        assert!(
            loaded.search(&[1.0, 0.0], 5, None).is_empty(),
            "wrong-dim query answers empty after load"
        );
        assert_eq!(loaded.search(&[1.0, 0.0, 0.5], 5, None).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tantivy_text_index_orders_and_removes() {
        let idx = TantivyTextIndex::new().unwrap();
        let a = kid(1);
        let b = kid(2);
        idx.upsert(a, &BTreeSet::from(["cats".to_string(), "dogs".to_string()]))
            .unwrap();
        idx.upsert(b, &BTreeSet::from(["birds".to_string()]))
            .unwrap();
        let r = idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, a);
        idx.remove(&a).unwrap();
        assert!(idx
            .search(&BTreeSet::from(["cats".to_string()]), 5)
            .unwrap()
            .is_empty());
    }

    // --- P4-M7 (TDD-VECTOR-001) — vec001: batch ops answer == per-item
    // answers. RED: `upsert_many`/`remove_many` do not exist yet. ---

    #[test]
    fn vec001_upsert_many_answers_equal_per_item() {
        let items = vec![
            (
                kid(1),
                BTreeSet::from(["cats".to_string(), "dogs".to_string()]),
            ),
            (kid(2), BTreeSet::from(["birds".to_string()])),
            (kid(3), BTreeSet::from(["fish".to_string()])),
        ];
        let batched = TantivyTextIndex::new().unwrap();
        batched.upsert_many(&items).unwrap();
        let per_item = TantivyTextIndex::new().unwrap();
        for (k, t) in &items {
            per_item.upsert(*k, t).unwrap();
        }
        assert_eq!(batched.len(), 3);
        assert_eq!(batched.len(), per_item.len());
        for probe in ["cats", "birds", "fish"] {
            let q = BTreeSet::from([probe.to_string()]);
            assert_eq!(
                batched.search(&q, 5).unwrap(),
                per_item.search(&q, 5).unwrap(),
                "batch answers == per-item answers for {probe}"
            );
        }
    }

    #[test]
    fn vec001_remove_many_answers_equal_per_item() {
        let fill = |idx: &TantivyTextIndex| {
            for i in 1..=3u8 {
                idx.upsert(kid(i), &BTreeSet::from([format!("t{i}")]))
                    .unwrap();
            }
        };
        let batched = TantivyTextIndex::new().unwrap();
        fill(&batched);
        batched.remove_many(&[kid(1), kid(2)]).unwrap();
        let per_item = TantivyTextIndex::new().unwrap();
        fill(&per_item);
        per_item.remove(&kid(1)).unwrap();
        per_item.remove(&kid(2)).unwrap();
        assert_eq!(batched.len(), 1);
        assert_eq!(batched.len(), per_item.len());
        assert!(batched
            .search(&BTreeSet::from(["t1".to_string()]), 5)
            .unwrap()
            .is_empty());
        assert_eq!(
            batched
                .search(&BTreeSet::from(["t3".to_string()]), 5)
                .unwrap(),
            per_item
                .search(&BTreeSet::from(["t3".to_string()]), 5)
                .unwrap()
        );
    }

    /// A repeated koid within one batch: the last write wins, one doc stays.
    #[test]
    fn vec001_batch_repeat_koid_last_write_wins() {
        let idx = TantivyTextIndex::new().unwrap();
        idx.upsert_many(&[
            (kid(1), BTreeSet::from(["cats".to_string()])),
            (kid(1), BTreeSet::from(["dogs".to_string()])),
        ])
        .unwrap();
        assert_eq!(idx.len(), 1, "no duplicate docs for the same koid");
        assert!(
            idx.search(&BTreeSet::from(["cats".to_string()]), 5)
                .unwrap()
                .is_empty(),
            "the earlier write is superseded"
        );
        assert_eq!(
            idx.search(&BTreeSet::from(["dogs".to_string()]), 5)
                .unwrap()
                .len(),
            1
        );
    }

    // --- P4-M7 (TDD-VECTOR-002) — vec002: HNSW health + dead-ratio rebuild.
    // RED: `health`/`maybe_rebuild` do not exist yet. ---

    #[test]
    fn vec002_health_metrics_and_dead_ratio_rebuild_once() {
        let idx = HnswVectorIndex::new(2, 100);
        // Distinct directions: [i, 1] vectors are NOT collinear, so the
        // cosine ranking is well-defined (collinear probes would tie).
        for i in 1..=10u8 {
            idx.upsert(kid(i), "m", &[i as f32, 1.0]);
        }
        for i in 1..=5u8 {
            idx.remove(&kid(i));
        }
        let h = idx.health().expect("HNSW reports health");
        assert_eq!(h.live, 5);
        assert_eq!(
            h.physical, 10,
            "removes tombstone, the graph keeps the node"
        );
        assert_eq!(h.tombstones, 5);
        assert_eq!(h.dead_ratio, 0.5);

        assert!(idx.maybe_rebuild(), "ratio 0.5 > threshold — rebuild runs");
        assert!(!idx.maybe_rebuild(), "the rebuild is ONCE, not per call");
        let h2 = idx.health().unwrap();
        assert_eq!(h2.tombstones, 0);
        assert_eq!(h2.dead_ratio, 0.0);
        assert_eq!(h2.live, 5);
        assert_eq!(h2.physical, 5, "the graph shrank to the live set");

        let r = idx.search(&[9.0, 1.0], 10, None);
        assert!(
            r.iter().all(|(k, _)| *k != kid(1)),
            "dead entries stay excluded"
        );
        assert_eq!(r[0].0, kid(9), "live entries survive the rebuild");

        idx.remove(&kid(6)); // 1/5 = 0.2 — below the threshold
        assert!(
            !idx.maybe_rebuild(),
            "below-threshold deletes never rebuild"
        );
    }

    // --- P5-M27 (IDX-P1-04) — the rebuild trigger survives. RED: a post-load
    // index bails on empty vectors but the flag is already consumed, so the
    // rehydrated index never rebuilds; and a checkpoint round-trip loses the
    // flag entirely (the dead ratio stays crossed, no later delete re-arms
    // it). ---

    #[test]
    fn vec004_post_load_rebuild_trigger_survives_unavailable_vectors() {
        // justified: test-thread names contain `::` (invalid on Windows paths)
        let dir = std::env::temp_dir().join(format!("aikoql-vec004-a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let idx = HnswVectorIndex::new(2, 100);
            for i in 1..=10u8 {
                idx.upsert(kid(i), "m", &[i as f32, 1.0]);
            }
            idx.checkpoint(&dir).unwrap();
        }
        let loaded = HnswVectorIndex::load(&dir).unwrap();
        // Post-load state: the map knows the pairs, the vectors are empty.
        for i in 1..=5u8 {
            loaded.remove(&kid(i)); // 5/10 — crosses the rebuild ratio
        }
        assert!(
            !loaded.maybe_rebuild(),
            "vectors unavailable — the rebuild cannot run yet"
        );
        // The maintainer's catch-up re-upserts the LIVE entries only.
        for i in 6..=10u8 {
            loaded.upsert(kid(i), "m", &[i as f32, 1.0]);
        }
        assert!(
            loaded.maybe_rebuild(),
            "the trigger survived the unavailable-vectors bail — rebuild now"
        );
        let h = loaded.health().unwrap();
        assert_eq!(h.tombstones, 0);
        assert_eq!(h.physical, 5, "the graph shrank to the live set");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vec004_pending_rebuild_survives_checkpoint_roundtrip() {
        // justified: test-thread names contain `::` (invalid on Windows paths)
        let dir = std::env::temp_dir().join(format!("aikoql-vec004-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let idx = HnswVectorIndex::new(2, 100);
            for i in 1..=10u8 {
                idx.upsert(kid(i), "m", &[i as f32, 1.0]);
            }
            for i in 1..=5u8 {
                idx.remove(&kid(i)); // arms the trigger — no maintenance tick yet
            }
            idx.checkpoint(&dir).unwrap();
        }
        let loaded = HnswVectorIndex::load(&dir).unwrap();
        for i in 6..=10u8 {
            loaded.upsert(kid(i), "m", &[i as f32, 1.0]); // rehydrate the live
        }
        assert!(
            loaded.maybe_rebuild(),
            "the armed trigger survived the checkpoint round-trip"
        );
        let h = loaded.health().unwrap();
        assert_eq!(h.tombstones, 0);
        assert_eq!(h.physical, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- CI fix (Python SDK close→reopen) — an empty tantivy checkpoint
    // (the maintainer checkpointed before the first note seeded tokens)
    // loads without handing tantivy a limit of 0: TopDocs::with_limit(0)
    // panics. RED: load() panics on the empty index. ---

    #[test]
    fn text_empty_checkpoint_loads_without_panicking() {
        // justified: test-thread names contain `::` (invalid on Windows paths)
        let dir = std::env::temp_dir().join(format!("aikoql-tvec-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TantivyTextIndex::new().unwrap().checkpoint(&dir).unwrap();
        let loaded = TantivyTextIndex::load(&dir).unwrap();
        assert!(
            loaded
                .search(&BTreeSet::from(["anything".to_string()]), 5)
                .unwrap()
                .is_empty(),
            "the empty checkpoint answers no documents"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- P5-M27 (IDX-P1-03) — dimension mismatches are observable. RED: the
    // health struct has no mismatch signal — E0609, the missing-seam RED (the
    // cbo_default_001 pattern). The wrong-dim drop is behavior-pinned at
    // vec003; this pins the observability channel. ---

    #[test]
    fn vec005_dim_mismatch_is_observable_in_health() {
        let idx = HnswVectorIndex::new(2, 100);
        idx.upsert(kid(1), "m", &[1.0, 0.0]);
        idx.upsert(kid(2), "m", &[0.5, 0.5, 0.25]); // wrong dim — dropped
        idx.upsert(kid(3), "n", &[0.1, 0.9, 0.2]); // wrong dim — dropped
        let h = idx.health().unwrap();
        assert_eq!(
            h.dropped_dim_mismatch, 2,
            "mismatches are counted, never silently 'healthy'"
        );
        assert_eq!(h.live, 1, "only the adopted-dim vector is live");
    }
}
