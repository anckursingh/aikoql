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

impl TenantManager {
    pub fn new() -> Self {
        TenantManager {
            quotas: RwLock::new(HashMap::new()),
            usage: RwLock::new(HashMap::new()),
        }
    }

    /// Set or update a tenant's quota.
    pub fn set_quota(&self, tenant: &str, quota: TenantQuota) {
        self.quotas.write().unwrap().insert(tenant.into(), quota);
    }

    /// Get current object count for a tenant, or 0 if unknown.
    pub fn usage(&self, tenant: &str) -> usize {
        self.usage.read().unwrap().get(tenant).copied().unwrap_or(0)
    }

    /// Rebuild usage counters from a full head scan. Called on startup.
    pub fn rebuild(&self, heads: &[(KOID, u64, u64, LifecycleState)], type_resolver: impl Fn(&KOID) -> Option<String>) {
        let mut usage = self.usage.write().unwrap();
        usage.clear();
        for (koid, _, _, state) in heads {
            if *state == LifecycleState::Deleted {
                continue;
            }
            // Determine tenant by loading the KO — ponytail: O(n) scan,
            // amortized over startup. For hundreds of tenants, add a tenant
            // index prefix when this becomes a bottleneck.
            if let Some(_type_name) = type_resolver(koid) {
                // ponytail: tenant stored in KO metadata; for now use "default".
                let t = "default".to_string();
                *usage.entry(t).or_insert(0) += 1;
            }
        }
    }

    /// Check whether a create/update would exceed the tenant's quota.
    /// Returns `Ok(())` if allowed, `Err` with the limit otherwise.
    pub fn check_create(&self, _tenant: Option<&str>) -> KResult<()> {
        let t = _tenant.unwrap_or("default");
        let quota = self
            .quotas
            .read()
            .unwrap()
            .get(t)
            .cloned()
            .unwrap_or_default();
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
        *self.usage.write().unwrap().entry(t).or_insert(0) += 1;
    }

    /// Record a deletion for the tenant.
    pub fn record_delete(&self, _tenant: Option<&str>) {
        let t = _tenant.unwrap_or("default").to_string();
        if let Some(c) = self.usage.write().unwrap().get_mut(&t) {
            *c = c.saturating_sub(1);
        }
    }
}
