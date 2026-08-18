//! Rate limiting for MCP tool calls and the REST surface.
//!
//! ## Scope
//!
//! `RateLimiter` is **process-local**: each OS process maintains its own
//! counters, so N instances each allow the configured limit (aggregate
//! N × limit). For global limiting across instances, enforce at the
//! gateway/load-balancer layer or swap the counters for a shared backend.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Sliding-window rate limiter: at most `max_per_window` calls per
/// `window_secs` per key. Fixed epochs (`now / window * window`), so all
/// keys roll over at the same wall-clock instant.
pub(crate) struct RateLimiter {
    enabled: bool,
    max_per_window: u64,
    window_secs: u64,
    counters: HashMap<String, (u64, u64)>, // (count, window_start)
}

impl RateLimiter {
    pub(crate) fn new(enabled: bool, max_per_minute: u64) -> Self {
        RateLimiter {
            enabled,
            max_per_window: max_per_minute.max(1),
            window_secs: 60,
            counters: HashMap::new(),
        }
    }

    /// Record one call for `key`; `Err(max)` when the window is exhausted.
    pub(crate) fn check(&mut self, key: &str) -> Result<(), u64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            // justified: pre-epoch clock is unreachable in practice
            .unwrap_or(0);
        self.check_at(key, now)
    }

    pub(crate) fn check_at(&mut self, key: &str, now_secs: u64) -> Result<(), u64> {
        if !self.enabled {
            return Ok(());
        }
        let start = now_secs / self.window_secs * self.window_secs;
        let (count, window) = self.counters.entry(key.to_string()).or_insert((0, start));
        if *window != start {
            *count = 0;
            *window = start;
        }
        *count += 1;
        if *count > self.max_per_window {
            return Err(self.max_per_window);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit() {
        let mut rl = RateLimiter::new(true, 3);
        assert!(rl.check_at("k", 1000).is_ok());
        assert!(rl.check_at("k", 1000).is_ok());
        assert!(rl.check_at("k", 1000).is_ok());
        assert_eq!(rl.check_at("k", 1000), Err(3));
    }

    #[test]
    fn window_rollover_resets() {
        let mut rl = RateLimiter::new(true, 2);
        assert!(rl.check_at("k", 60).is_ok());
        assert!(rl.check_at("k", 61).is_ok());
        assert_eq!(rl.check_at("k", 65), Err(2));
        assert!(rl.check_at("k", 120).is_ok()); // new epoch
    }

    #[test]
    fn keys_are_independent() {
        let mut rl = RateLimiter::new(true, 1);
        assert!(rl.check_at("a", 0).is_ok());
        assert!(rl.check_at("b", 0).is_ok());
    }

    #[test]
    fn disabled_never_blocks() {
        let mut rl = RateLimiter::new(false, 1);
        for _ in 0..100 {
            assert!(rl.check_at("k", 0).is_ok());
        }
    }
}
