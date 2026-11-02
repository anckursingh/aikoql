//! Tenant Manager — multi-tenancy quota enforcement (MRFC-0005 §Enterprise).
//!
//! Tracks per-tenant object counts and enforces configurable limits.
//! AG-32b: the live counters persist as one KV row per tenant under
//! `USAGE_PREFIX` (an 8-byte LE count), and a `(seq, ts)` head watermark
//! (`USAGE_HEAD_KEY`) rides every commit batch atomically with the rows.
//! A clean open loads the rows when the watermark matches the journal
//! head — no head decode; anything else rebuilds from the heads and
//! re-persists both (see `Kernel::open_with`).

use crate::knowledge::kom::*;
use std::collections::HashMap;
use std::sync::RwLock;

/// AG-32b — the KV row prefix for per-tenant usage counts.
pub const USAGE_PREFIX: &[u8] = b"__tenants/usage/";
/// AG-32b — the (seq, ts) watermark of the head the usage rows are
/// exact at. It rides every commit batch, so it matches the journal
/// head exactly when the rows are trustworthy.
pub const USAGE_HEAD_KEY: &[u8] = b"__tenants/usage-head";

/// The KV row key for a tenant's usage count.
pub fn usage_row_key(tenant: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(USAGE_PREFIX.len() + tenant.len());
    k.extend_from_slice(USAGE_PREFIX);
    k.extend_from_slice(tenant.as_bytes());
    k
}

/// The watermark bytes for journal head `(seq, ts)`.
pub fn usage_head(seq: u64, ts: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&ts.to_le_bytes());
    v
}

/// Per-tenant resource limits.
#[derive(Clone, Debug)]
pub struct TenantQuota {
    pub max_objects: usize,
}

impl Default for TenantQuota {
    fn default() -> Self {
        TenantQuota {
            // the default for an explicitly constructed quota; unconfigured
            // tenants are unlimited (see check_create)
            max_objects: 10_000,
        }
    }
}

/// Tracks usage per tenant and enforces quotas.
pub struct TenantManager {
    quotas: RwLock<HashMap<String, TenantQuota>>,
    usage: RwLock<HashMap<String, usize>>,
}

impl Default for TenantManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantManager {
    pub fn new() -> Self {
        TenantManager {
            quotas: RwLock::new(HashMap::new()),
            usage: RwLock::new(HashMap::new()),
        }
    }

    /// Set or update a tenant's quota.
    pub fn set_quota(&self, tenant: &str, quota: TenantQuota) {
        // justified: RwLock poison is unrecoverable
        self.quotas.write().unwrap().insert(tenant.into(), quota);
    }

    /// Get current object count for a tenant, or 0 if unknown.
    pub fn usage(&self, tenant: &str) -> usize {
        // justified: RwLock poison is unrecoverable; absent usage count → 0
        self.usage.read().unwrap().get(tenant).copied().unwrap_or(0)
    }

    /// Rebuild usage counters from a full head scan. Called on startup.
    /// AG-32: `tenant_resolver` returns each head's own `metadata.tenant` —
    /// a tenant-less head counts under "default" (the same rule as
    /// `check_create`/`record_create`).
    pub fn rebuild(
        &self,
        heads: &[(KOID, u64, u64, LifecycleState)],
        // (koid, commit_ts) — the resolver reads the head's metadata, which
        // needs the version timestamp (get_object_version)
        tenant_resolver: impl Fn(&KOID, u64) -> Option<String>,
    ) {
        // justified: RwLock poison is unrecoverable
        let mut usage = self.usage.write().unwrap();
        usage.clear();
        for (koid, _version, ts, state) in heads {
            if *state == LifecycleState::Deleted {
                continue;
            }
            let t = tenant_resolver(koid, *ts).unwrap_or_else(|| "default".to_string());
            *usage.entry(t).or_insert(0) += 1;
        }
    }

    /// Check whether a create/update would exceed the tenant's quota.
    /// Returns `Ok(())` if allowed, `Err` with the limit otherwise.
    pub fn check_create(&self, _tenant: Option<&str>) -> KResult<()> {
        let t = _tenant.unwrap_or("default");
        // justified: RwLock poison is unrecoverable; unset quota → unlimited
        // (a default cap on every tenant broke bulk loads: cbo_default_007
        // seeds 10 001 objects on the default tenant)
        let Some(quota) = self.quotas.read().unwrap().get(t).cloned() else {
            return Ok(());
        };
        // justified: RwLock poison is unrecoverable; absent usage count → 0
        let current = self.usage.read().unwrap().get(t).copied().unwrap_or(0);
        if current >= quota.max_objects {
            return Err(KError::InvalidObject(format!(
                "tenant '{}' quota exceeded: {} objects (max {})",
                t, current, quota.max_objects
            )));
        }
        Ok(())
    }

    /// Record a new object for the tenant.
    pub fn record_create(&self, _tenant: Option<&str>) {
        let t = _tenant.unwrap_or("default").to_string();
        // justified: RwLock poison is unrecoverable
        *self.usage.write().unwrap().entry(t).or_insert(0) += 1;
    }

    /// Record a deletion for the tenant.
    pub fn record_delete(&self, _tenant: Option<&str>) {
        let t = _tenant.unwrap_or("default").to_string();
        // justified: RwLock poison is unrecoverable
        if let Some(c) = self.usage.write().unwrap().get_mut(&t) {
            *c = c.saturating_sub(1);
        }
    }

    /// AG-32b — the live map, for persisting after a rebuild.
    pub fn usage_rows(&self) -> Vec<(String, usize)> {
        // justified: RwLock poison is unrecoverable
        self.usage
            .read()
            .unwrap()
            .iter()
            .map(|(t, n)| (t.clone(), *n))
            .collect()
    }

    /// AG-32b — replace the live map (a clean open loads the persisted
    /// rows instead of decoding heads).
    pub fn load(&self, rows: Vec<(String, usize)>) {
        // justified: RwLock poison is unrecoverable
        *self.usage.write().unwrap() = rows.into_iter().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn koid(b: u8) -> KOID {
        KOID::from_bytes([b; KOID_LEN])
    }

    /// AG-32 gate: rebuild counts each head under its OWN metadata tenant,
    /// not the hard-coded "default".
    #[test]
    fn ag32_rebuild_counts_per_metadata_tenant() {
        let tm = TenantManager::new();
        let heads = vec![
            (koid(1), 1, 1, LifecycleState::Active),
            (koid(2), 1, 1, LifecycleState::Active),
            (koid(3), 1, 1, LifecycleState::Active),
            (koid(4), 1, 1, LifecycleState::Deleted), // deleted: never counted
            (koid(5), 1, 1, LifecycleState::Active),
        ];
        let tenants = HashMap::from([
            (koid(1), "a".to_string()),
            (koid(2), "a".to_string()),
            (koid(3), "b".to_string()),
            (koid(5), "b".to_string()),
        ]);
        tm.rebuild(&heads, |k, _ts| tenants.get(k).cloned());
        assert_eq!(tm.usage("a"), 2);
        assert_eq!(tm.usage("b"), 2);
        assert_eq!(tm.usage("default"), 0);
        assert_eq!(tm.usage("other"), 0);
    }

    /// AG-32: a tenant-less head counts under the default tenant.
    #[test]
    fn ag32_rebuild_tenantless_counts_default() {
        let tm = TenantManager::new();
        let heads = vec![(koid(1), 1, 1, LifecycleState::Active)];
        tm.rebuild(&heads, |_, _| None);
        assert_eq!(tm.usage("default"), 1);
    }

    /// Unconfigured tenants are unlimited (a default cap broke bulk loads:
    /// cbo_default_007 seeds 10 001 objects); only an explicit set_quota
    /// enforces.
    #[test]
    fn ag32_unconfigured_tenant_is_unlimited_until_a_quota_is_set() {
        let tm = TenantManager::new();
        for _ in 0..20_000 {
            tm.record_create(None);
        }
        assert!(tm.check_create(None).is_ok(), "unlimited by default");
        tm.set_quota("default", TenantQuota { max_objects: 5 });
        assert!(
            tm.check_create(None).is_err(),
            "an explicit quota must be enforced"
        );
    }
}
