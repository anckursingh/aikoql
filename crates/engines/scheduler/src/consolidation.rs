//! AG-13 — memory consolidation: merge near-duplicate KOs so the kernel
//! surface stays small without losing provenance. A service around the
//! kernel, never on the commit path: the job runs on a cadence and every
//! merge is an ordinary kernel transaction (derive with `derived_from`
//! edges to every source — lineage kept, AG-13a).
//!
//! The gate is conservative: two KOs of the same type and tenant are
//! near-duplicates when their property similarity (token Jaccard for text
//! values, equality otherwise) reaches the threshold and neither is
//! already a `derived_from` source. Consolidation NEVER deletes — sources
//! stay readable and only `forget` deletes (AG-13c).

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{Kernel, KnowledgeContext, Subject};
use aikoql_kernel::{MergeRequest, MergeStrategy};
use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default property-similarity gate.
/// shortcut: no config knob — add a flag when ops tune it against the
/// AG-61 eval measurements (recall@10 vs. surface size).
pub const DEFAULT_THRESHOLD: f64 = 0.8;

/// Default cadence. shortcut: no config knob — same tuning story.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// AG-13: merge near-duplicate KOs type by type. Returns the KOIDs of the
/// derived (merged) KOs. Idempotent across runs: merged sources carry
/// `derived_from` edges and are skipped thereafter. Never deletes.
pub fn consolidate(k: &Kernel, subject: &Subject, threshold: f64) -> KResult<Vec<KOID>> {
    let mut merged = Vec::new();
    for type_name in k.list_types()? {
        let kos = k.scan_by_type(subject, &type_name)?;
        let mut used: HashSet<KOID> = HashSet::new();
        'pairs: for i in 0..kos.len() {
            for j in (i + 1)..kos.len() {
                let (a, b) = (&kos[i], &kos[j]);
                if !is_mergeable(k, a, b, &used, threshold)? {
                    continue;
                }
                let mut req = MergeRequest::new(
                    KnowledgeContext::new(subject.clone()),
                    type_name.clone(),
                    vec![a.koid, b.koid],
                );
                req.strategy = MergeStrategy::NewestWins;
                req.reason = Some("ag-13 consolidation: near-duplicate merge".into());
                let r = k.merge(req)?;
                merged.push(r.koid);
                used.insert(a.koid);
                used.insert(b.koid);
                // shortcut: one merge per outer KO per run — a wider
                // duplicate cluster consolidates over successive runs
                break 'pairs;
            }
        }
    }
    Ok(merged)
}

/// Both sides must be current claims, same tenant, not already merge
/// sources (this run or any earlier one), and near-duplicate by property
/// similarity.
fn is_mergeable(
    k: &Kernel,
    a: &KnowledgeObject,
    b: &KnowledgeObject,
    used: &HashSet<KOID>,
    threshold: f64,
) -> KResult<bool> {
    if used.contains(&a.koid) || used.contains(&b.koid) {
        return Ok(false);
    }
    if a.metadata.tenant != b.metadata.tenant {
        return Ok(false);
    }
    if !is_current(a) || !is_current(b) {
        return Ok(false);
    }
    // A source of an earlier merge is never a merge source again — this is
    // what makes consolidate idempotent across runs.
    if !k.outbound_edges(&a.koid, Some(DERIVED_FROM))?.is_empty()
        || !k.outbound_edges(&b.koid, Some(DERIVED_FROM))?.is_empty()
    {
        return Ok(false);
    }
    Ok(similarity(&a.properties, &b.properties) >= threshold)
}

fn is_current(ko: &KnowledgeObject) -> bool {
    // Draft is the default state of a remembered KO — the bulk of the
    // store. Skip only what is gone or replaced.
    !matches!(
        ko.lifecycle.state,
        LifecycleState::Archived | LifecycleState::Deleted | LifecycleState::Superseded
    ) && !matches!(
        ko.epistemic_status(),
        EpistemicStatus::Contradicted | EpistemicStatus::Superseded
    )
}

/// Mean per-key similarity over the union of keys: token Jaccard for text
/// values (case-insensitive), equality for the rest.
fn similarity(a: &PropertyMap, b: &PropertyMap) -> f64 {
    let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
    if keys.is_empty() {
        return 0.0;
    }
    let sum: f64 = keys
        .iter()
        .map(|key| match (a.get(*key), b.get(*key)) {
            (Some(Value::Text(x)), Some(Value::Text(y))) => token_jaccard(x, y),
            (Some(x), Some(y)) => i64::from(x == y) as f64,
            _ => 0.0,
        })
        .sum();
    sum / keys.len() as f64
}

fn token_jaccard(x: &str, y: &str) -> f64 {
    // shortcut: whitespace tokens, lowercased — the text index tokenizer is
    // fancier; upgrade when consolidation proves too strict in the eval run
    let tx: BTreeSet<_> = x.split_whitespace().map(str::to_lowercase).collect();
    let ty: BTreeSet<_> = y.split_whitespace().map(str::to_lowercase).collect();
    if tx.is_empty() && ty.is_empty() {
        return 1.0;
    }
    let inter = tx.intersection(&ty).count();
    let union = tx.union(&ty).count();
    inter as f64 / union as f64
}

/// AG-13: the cadence job — consolidates once `interval` has elapsed since
/// the last run and the journal has advanced past the last consolidated
/// water. A quiet journal costs nothing.
pub struct ConsolidationJob {
    stop: Arc<AtomicBool>,
    water: Arc<AtomicU64>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ConsolidationJob {
    /// Start the background thread (like `IndexMaintainer::start`). The job
    /// subject is an unscoped admin — the only principal the kernel lets
    /// read across tenants (auth T-55).
    pub fn start(kernel: &Kernel, interval: Duration) -> KResult<Arc<Self>> {
        let subject = Subject::with_roles("consolidation", &["admin"]);
        let k = kernel.clone_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let (head, _) = kernel.journal_head()?;
        let water = Arc::new(AtomicU64::new(head));
        let (stop_w, water_w) = (stop.clone(), water.clone());
        let handle = std::thread::spawn(move || {
            let mut last = std::time::Instant::now();
            while !stop_w.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if last.elapsed() < interval {
                    continue;
                }
                let Ok((head, _)) = k.journal_head() else {
                    continue;
                };
                if head <= water_w.load(Ordering::Relaxed) {
                    continue;
                }
                last = std::time::Instant::now();
                match consolidate(&k, &subject, DEFAULT_THRESHOLD) {
                    Ok(merged) => {
                        water_w.store(head, Ordering::Relaxed);
                        if !merged.is_empty() {
                            eprintln!(
                                "ag-13: consolidated {} near-duplicate(s), latest {}",
                                merged.len(),
                                merged.last().unwrap().to_hex()
                            );
                        }
                    }
                    Err(e) => eprintln!("ag-13: consolidation failed: {e}"),
                }
            }
        });
        Ok(Arc::new(Self {
            stop,
            water,
            handle: Mutex::new(Some(handle)),
        }))
    }

    /// Stop the background thread and join it.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    /// Last consolidated journal water.
    pub fn water(&self) -> u64 {
        self.water.load(Ordering::Relaxed)
    }
}
