//! Tenant Manager — multi-tenancy quota enforcement (MRFC-0005 §Enterprise).
//!
//! Tracks per-tenant object counts and enforces configurable limits.
//! ponytail: in-memory counters rebuilt from journal on startup.
//! Persistent per-tenant watermark would need a KV prefix; add when
//! tenants number in the hundreds.

use crate::knowledge::kom::*;
use std::collections::HashMap;
use std::sync::RwLock;

/// Per-tenant resource limits.
#[derive(Clone, Debug)]
pub struct TenantQuota {
    pub max_objects: usize,
}

impl Default for TenantQuota {
    fn default() -> Self {
        TenantQuota {
            max_objects: 10_000, // generous default
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
        tenant_resolver: impl Fn(&KOID) -> Option<String>,
    ) {
        // justified: RwLock poison is unrecoverable
        let mut usage = self.usage.write().unwrap();
        usage.clear();
        for (koid, _, _, state) in heads {
            if *state == LifecycleState::Deleted {
                continue;
            }
            let t = tenant_resolver(koid).unwrap_or_else(|| "default".to_string());
            *usage.entry(t).or_insert(0) += 1;
        }
    }

    /// Check whether a create/update would exceed the tenant's quota.
    /// Returns `Ok(())` if allowed, `Err` with the limit otherwise.
    pub fn check_create(&self, _tenant: Option<&str>) -> KResult<()> {
        let t = _tenant.unwrap_or("default");
        let quota = self
            .quotas
            .read()
            // justified: RwLock poison is unrecoverable
            .unwrap()
            .get(t)
            .cloned()
            // justified: unset quota → TenantQuota::default()
            // (policy: default quota applies to unconfigured tenants)
            .unwrap_or_default();
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
        tm.rebuild(&heads, |k| tenants.get(k).cloned());
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
        tm.rebuild(&heads, |_| None);
        assert_eq!(tm.usage("default"), 1);
    }
}
