//! Rate limiting for MCP tool calls.
//!
//! ## Scope
//!
//! The default `InMemoryRateLimiter` is **process-local**. Each OS process
//! maintains its own independent counter. In a horizontally-scaled deployment
//! (load balancer → N instances), every instance independently allows the
//! configured limit — the aggregate call rate across instances is N × limit.
//!
//! For global rate limiting across instances, implement the `RateLimiter`
//! trait with a shared backend (Redis, Memcached, etc.) or enforce limits at
//! the gateway/load-balancer layer.
//!
//! ## Future
//!
//! The trait exists so the process-local impl can be swapped for a
//! distributed one without changing the call site. See `RateLimiter` trait.

use std::collections::HashMap;

/// Shared rate-limiter contract.
///
/// Implementations must be `Send + Sync` for concurrent use across
/// connection-handler tasks.
pub trait RateLimiter: Send + Sync {
    /// Record one call for `key` and return whether it is allowed.
    /// Returns `true` if under the limit, `false` if the limit is exceeded.
    fn check(&mut self, key: &str) -> bool;

    /// Reset the counter for `key` (e.g. after a window rolls over).
    fn reset(&mut self, key: &str);

    /// Current count for `key`.
    fn count(&self, key: &str) -> u64;
}

/// Process-local, in-memory rate limiter with no external dependencies.
///
/// Counts calls per key in a `HashMap`. Callers must periodically reset
/// counts (e.g. every 60s) to implement a sliding window.
pub struct InMemoryRateLimiter {
    max_per_window: u64,
    counters: HashMap<String, u64>,
}

impl InMemoryRateLimiter {
    pub fn new(max_per_window: u64) -> Self {
        InMemoryRateLimiter {
            max_per_window,
            counters: HashMap::new(),
        }
    }
}

impl RateLimiter for InMemoryRateLimiter {
    fn check(&mut self, key: &str) -> bool {
        let count = self.counters.entry(key.to_string()).or_insert(0);
        *count += 1;
        *count <= self.max_per_window
    }

    fn reset(&mut self, key: &str) {
        self.counters.remove(key);
    }

    fn count(&self, key: &str) -> u64 {
        self.counters.get(key).copied().unwrap_or(0)
    }
}
