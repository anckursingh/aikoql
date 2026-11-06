//! P5-M9 (ND-08) — statistics collection: the optimizer's ground truth.
//!
//! `analyze` walks the canonical heads (the same reconciliation walk the M8
//! verify/rebuild use — raw objects, no ACL) and computes the roadmap's list:
//! type cardinality, property cardinality (distinct counts), selectivity
//! (the uniform match fraction 1/distinct), relationship degree (avg outbound
//! fanout), vector candidate density, temporal density, tenant distribution.
//! The result persists as an ordinary catalog row (kind `statistics`, name =
//! type name — M7 made statistics one of the entities M8/M9 consume
//! transactionally) and a watermark: the journal length at capture. Any later
//! journal event makes the row stale, and a stale row must never silently
//! drive a plan (cbo008 pins the runtime fallback). Corrupt rows fail the
//! read closed (cbo_a04 — the M7 fail-closed discipline).

use crate::index::property::encode;
use crate::knowledge::kom::*;
use crate::transaction::kernel::Kernel;
use std::collections::{BTreeMap, HashMap, HashSet};

/// AG-21d — the fixed bucket count of a range histogram. Equal-depth over
/// the property's DISTINCT ordered keys; the per-bucket row count carries
/// the skew, so the estimate stays honest when hot keys clump.
pub const HISTOGRAM_BUCKETS: usize = 64;

/// One type's statistics snapshot, as persisted in the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct Statistics {
    pub type_name: String,
    /// Type cardinality: live (non-tombstoned) rows of the type.
    pub row_count: u64,
    /// Property cardinality: distinct values per property.
    pub distinct: BTreeMap<String, u64>,
    /// AG-21d — per-property range histograms over the index's ordered
    /// keys: (bucket start key, rows in the bucket). Only properties whose
    /// values all fall in the orderable domains (Num/Text/Bool) and have
    /// at least HISTOGRAM_BUCKETS distinct values carry one.
    pub histograms: BTreeMap<String, Vec<(Vec<u8>, u64)>>,
    /// Relationship degree: avg outbound edges per row.
    pub fanout: f64,
    /// Vector candidate density: rows with an embedding / rows.
    pub vector_density: f64,
    /// Temporal density: rows with valid-time bounds / rows.
    pub temporal_density: f64,
    /// Tenant distribution: distinct tenants among the rows.
    pub tenant_count: u64,
    /// The journal length at capture — the row's own create event sits at
    /// watermark-1, so any LATER event makes the row stale.
    pub watermark: u64,
    pub captured_at: u64,
}

impl Statistics {
    /// Distinct values of one property (property cardinality).
    pub fn cardinality(&self, property: &str) -> Option<u64> {
        self.distinct.get(property).copied()
    }

    /// Selectivity = the fraction of rows matching one value, under the
    /// uniform assumption: 1/distinct. 0.0 when unknown (never analyzed).
    pub fn selectivity(&self, property: &str) -> f64 {
        match self.distinct.get(property) {
            Some(&d) if d > 0 => 1.0 / d as f64,
            _ => 0.0,
        }
    }

    /// AG-21d — the estimated fraction of the property's rows in
    /// `[lo, hi)` (None bound = unbounded), from the range histogram:
    /// fully-covered buckets count whole, the two boundary buckets count
    /// half. `None` when the property carries no histogram (never
    /// analyzed, unorderable values, or too few distinct values).
    pub fn range_fraction(
        &self,
        property: &str,
        lo: Option<&Value>,
        hi: Option<&Value>,
    ) -> Option<f64> {
        let buckets = self.histograms.get(property)?;
        let lo_key = lo.map(|v| encode(&[v]));
        let hi_key = hi.map(|v| encode(&[v]));
        // an empty range: no value can sit in [lo, hi)
        if let (Some(a), Some(b)) = (&lo_key, &hi_key) {
            if a >= b {
                return Some(0.0);
            }
        }
        let mut total: u64 = 0;
        let mut inside: u64 = 0;
        for (i, (start, count)) in buckets.iter().enumerate() {
            // the bucket covers [start, next_start); the last runs to +∞
            let end = buckets.get(i + 1).map(|(s, _)| s.as_slice());
            total += count;
            let start_ge_lo = lo_key
                .as_ref()
                .is_none_or(|k| start.as_slice() >= k.as_slice());
            let end_le_hi = match (end, hi_key.as_deref()) {
                (Some(e), Some(k)) => e <= k,
                (_, None) => true,        // +∞ cap: hi unbounded
                (None, Some(_)) => false, // the last bucket runs past hi
            };
            if start_ge_lo && end_le_hi {
                inside += count;
                continue;
            }
            // a boundary bucket: it straddles lo or hi — half its rows
            let crosses_lo = lo_key.as_ref().is_some_and(|k| {
                start.as_slice() < k.as_slice() && end.is_none_or(|e| e > k.as_slice())
            });
            let crosses_hi = hi_key.as_ref().is_some_and(|k| {
                start.as_slice() < k.as_slice() && end.is_none_or(|e| e > k.as_slice())
            });
            if crosses_lo || crosses_hi {
                inside += count / 2;
            }
        }
        Some(inside as f64 / total as f64)
    }

    /// Stale iff any journal event landed after the capture watermark.
    pub fn is_stale(&self, journal_len: u64) -> bool {
        journal_len > self.watermark
    }
}

/// Parse a statistics catalog row. Fails closed on any wrong shape/type —
/// a corrupt row must never silently feed the optimizer (cbo_a04).
fn parse_statistics(type_name: &str, props: &PropertyMap) -> KResult<Statistics> {
    let corrupt =
        |what: &str| KError::Store(format!("catalog statistics '{type_name}' corrupt: {what}"));
    let int = |key: &str| -> KResult<u64> {
        match props.get(key) {
            Some(Value::Int(v)) if *v >= 0 => Ok(*v as u64),
            other => Err(corrupt(&format!("{key} is {other:?}"))),
        }
    };
    let float = |key: &str| -> KResult<f64> {
        match props.get(key) {
            Some(Value::Float(f)) if *f >= 0.0 => Ok(*f),
            other => Err(corrupt(&format!("{key} is {other:?}"))),
        }
    };
    let mut distinct = BTreeMap::new();
    for (key, val) in props {
        if let Some(prop) = key.strip_prefix("distinct:") {
            match val {
                Value::Int(c) if *c >= 0 => {
                    distinct.insert(prop.to_string(), *c as u64);
                }
                other => return Err(corrupt(&format!("{key} is {other:?}"))),
            }
        }
    }
    // AG-21d: histograms persist per property as one Bytes payload of
    // (len-prefixed boundary key, bucket count) pairs — fail-closed like
    // every other field.
    let mut histograms = BTreeMap::new();
    for (key, val) in props {
        let Some(prop) = key.strip_prefix("hist:") else {
            continue;
        };
        let Value::Bytes(bytes) = val else {
            return Err(corrupt(&format!("{key} is {val:?}")));
        };
        let mut pos = 0usize;
        let mut buckets = Vec::new();
        let take = |pos: &mut usize, n: usize| -> KResult<&[u8]> {
            match bytes.get(*pos..*pos + n) {
                Some(s) => {
                    *pos += n;
                    Ok(s)
                }
                None => Err(corrupt(&format!("{key} truncated"))),
            }
        };
        while pos < bytes.len() {
            let len = u64::from_le_bytes(take(&mut pos, 8)?.try_into().unwrap()) as usize;
            let boundary = take(&mut pos, len)?.to_vec();
            let count = u64::from_le_bytes(take(&mut pos, 8)?.try_into().unwrap());
            buckets.push((boundary, count));
        }
        histograms.insert(prop.to_string(), buckets);
    }
    Ok(Statistics {
        type_name: type_name.into(),
        row_count: int("row_count")?,
        distinct,
        histograms,
        fanout: float("fanout")?,
        vector_density: float("vector_density")?,
        temporal_density: float("temporal_density")?,
        tenant_count: int("tenants")?,
        watermark: int("watermark")?,
        captured_at: int("captured_at")?,
    })
}

impl Kernel {
    /// Compute the type's statistics and persist them as a catalog row
    /// (kind `statistics`, name = type name). Re-analysis drops the old row
    /// first — one row per type. The watermark is captured between the drop
    /// and the create, so the row's own events never make it stale
    /// (single-writer; a concurrent commit only makes it stale — conservative).
    pub fn analyze(&self, type_name: &str) -> KResult<Statistics> {
        let mut row_count = 0u64;
        let mut distinct: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        // AG-21d: ordered-key counts per property (the index's encoding) +
        // whether every value is in an orderable domain — a histogram over
        // partial data would misprice a range.
        let mut key_counts: BTreeMap<String, HashMap<Vec<u8>, u64>> = BTreeMap::new();
        let mut orderable: BTreeMap<String, bool> = BTreeMap::new();
        let mut edges = 0u64;
        let mut embedded = 0u64;
        let mut temporal = 0u64;
        let mut tenants: HashSet<Option<String>> = HashSet::new();
        // ponytail: full head walk per analyze; analyze is an
        // explicit DBA operation, cache freshness if it becomes a hot path.
        for (koid, _version, ts, state) in self.scan_heads()? {
            if state == LifecycleState::Deleted {
                continue;
            }
            let Some(ko) = self.raw_object_at(&koid, ts)? else {
                continue;
            };
            if ko.metadata.type_name != type_name {
                continue;
            }
            row_count += 1;
            for (p, v) in &ko.properties {
                // Value is not Hash/Ord — the P5-M5 group-key Debug form.
                distinct
                    .entry(p.clone())
                    .or_default()
                    .insert(format!("{v:?}"));
                let ok = matches!(
                    v,
                    Value::Int(_) | Value::Float(_) | Value::Text(_) | Value::Bool(_)
                );
                *orderable.entry(p.clone()).or_insert(true) &= ok;
                if ok {
                    *key_counts
                        .entry(p.clone())
                        .or_default()
                        .entry(encode(&[v]))
                        .or_insert(0) += 1;
                }
            }
            edges += self.outbound_edges(&koid, None)?.len() as u64;
            if ko
                .semantic
                .as_ref()
                .and_then(|s| s.embedding.as_ref())
                .is_some()
            {
                embedded += 1;
            }
            if ko.valid_from().is_some() || ko.valid_to().is_some() {
                temporal += 1;
            }
            tenants.insert(ko.metadata.tenant.clone());
        }

        if self.catalog_entry("statistics", type_name)?.is_some() {
            self.catalog_drop_entry("statistics", type_name)?;
        }
        // AG-21d: equal-depth buckets over the sorted distinct keys; the
        // count per bucket is its rows, so the estimate carries the skew.
        // Skipped when the distinct values are too few for a histogram to
        // beat the uniform Eq selectivity (the CBO falls back to an exact
        // count probe there).
        let mut histograms = BTreeMap::new();
        for (p, counts) in &key_counts {
            if !orderable[p] || counts.len() < HISTOGRAM_BUCKETS {
                continue;
            }
            let mut keys: Vec<&Vec<u8>> = counts.keys().collect();
            keys.sort();
            let n = keys.len();
            let mut buckets = Vec::with_capacity(HISTOGRAM_BUCKETS);
            for b in 0..HISTOGRAM_BUCKETS {
                let s = b * n / HISTOGRAM_BUCKETS;
                let e = (b + 1) * n / HISTOGRAM_BUCKETS;
                let count = keys[s..e].iter().map(|k| counts[*k]).sum();
                buckets.push((keys[s].clone(), count));
            }
            histograms.insert(p.clone(), buckets);
        }
        let watermark = self.journal()?.len() as u64 + 1;
        let stats = Statistics {
            type_name: type_name.into(),
            row_count,
            distinct: distinct
                .into_iter()
                .map(|(p, s)| (p, s.len() as u64))
                .collect(),
            histograms,
            fanout: if row_count > 0 {
                edges as f64 / row_count as f64
            } else {
                0.0
            },
            vector_density: if row_count > 0 {
                embedded as f64 / row_count as f64
            } else {
                0.0
            },
            temporal_density: if row_count > 0 {
                temporal as f64 / row_count as f64
            } else {
                0.0
            },
            tenant_count: tenants.len() as u64,
            watermark,
            captured_at: self.clock_now(),
        };

        let mut props = PropertyMap::new();
        props.insert("row_count".into(), Value::Int(stats.row_count as i64));
        for (p, c) in &stats.distinct {
            // The prefix is injective over property names — no collisions.
            props.insert(format!("distinct:{p}"), Value::Int(*c as i64));
        }
        for (p, buckets) in &stats.histograms {
            let mut out = Vec::new();
            for (boundary, count) in buckets {
                out.extend_from_slice(&(boundary.len() as u64).to_le_bytes());
                out.extend_from_slice(boundary);
                out.extend_from_slice(&count.to_le_bytes());
            }
            props.insert(format!("hist:{p}"), Value::Bytes(out));
        }
        props.insert("fanout".into(), Value::Float(stats.fanout));
        props.insert("vector_density".into(), Value::Float(stats.vector_density));
        props.insert(
            "temporal_density".into(),
            Value::Float(stats.temporal_density),
        );
        props.insert("tenants".into(), Value::Int(stats.tenant_count as i64));
        props.insert("watermark".into(), Value::Int(stats.watermark as i64));
        props.insert("captured_at".into(), Value::Int(stats.captured_at as i64));
        self.catalog_create_entry("statistics", type_name, props)?;
        // P5-M17b: refresh the read cache with the freshly computed snapshot
        // (cbo_a06). A failed write leaves the old cached row in place —
        // the journal advanced, so it reads stale, never silently fresh.
        self.statistics_cache
            .write()
            .unwrap()
            .insert(type_name.into(), stats.clone());
        Ok(stats)
    }

    /// Read the type's statistics row. None = never analyzed; a corrupt row
    /// fails closed (cbo_a04).
    ///
    /// P5-M17b: cache-served — the default query path reads statistics per
    /// query, and the catalog lookup walks every head (measured ~3.5 ms on a
    /// 1k store). A miss scans the catalog ONCE per kernel per type and
    /// caches the parsed row; `analyze` refreshes the cache on write.
    /// Staleness stays watermark-judged against the journal head, so cache
    /// age can never promote a stale row (cbo_a06).
    pub fn statistics(&self, type_name: &str) -> KResult<Option<Statistics>> {
        // justified: RwLock poison is unrecoverable
        if let Some(s) = self.statistics_cache.read().unwrap().get(type_name) {
            return Ok(Some(s.clone()));
        }
        let Some(props) = self.catalog_entry("statistics", type_name)? else {
            return Ok(None);
        };
        let parsed = parse_statistics(type_name, &props)?;
        self.statistics_cache
            .write()
            .unwrap()
            .insert(type_name.into(), parsed.clone());
        Ok(Some(parsed))
    }
}
