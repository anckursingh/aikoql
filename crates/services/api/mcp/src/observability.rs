//! AG-50 observability: OTLP trace export, per-tool call metrics and the
//! recall quality counter.
//!
//! The seams are declared here so the RED tests can pin them:
//! - `record_tool_call` / `prometheus_tool_metrics` — one row per tool on
//!   the Prometheus scrape (calls, errors, latency sum).
//! - `record_recall` / `prometheus_recall_metrics` — the recall quality
//!   counter (empty results, conflict hits).
//! - the OTLP layer itself is wired in main.rs behind `AIKOQL_OTEL_ENDPOINT`
//!   and pinned end-to-end by tests/observability.rs (a trace for every
//!   tool call).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// One error per tool call moves the error counter, not the call count.
    #[test]
    fn ag50_tool_metrics_count_calls_errors_and_latency() {
        super::record_tool_call("remember", false, Duration::from_millis(3));
        super::record_tool_call("remember", true, Duration::from_millis(5));
        let stats = super::tool_stats();
        let remember = &stats["remember"];
        assert_eq!((remember.calls, remember.errors), (2, 1));
        assert!(remember.latency_ns >= 8_000_000, "latency must be summed");
    }

    /// The scrape renders one metric family per tool, labeled by tool.
    #[test]
    fn ag50_tool_metrics_render_prometheus_lines() {
        super::record_tool_call("recall", false, Duration::ZERO);
        let text = super::prometheus_tool_metrics();
        assert!(
            text.contains("aikoql_tool_calls_total{tool=\"recall\"} 1"),
            "got: {text}"
        );
        assert!(
            text.contains("aikoql_tool_errors_total{tool=\"recall\"} 0"),
            "got: {text}"
        );
    }

    /// Recall quality: empty result sets and surfaced conflicts both count,
    /// and both lines are always rendered (0 until the first occurrence).
    #[test]
    fn ag50_recall_quality_counts_empty_and_conflict() {
        super::record_recall(super::RecallOutcome::Empty);
        super::record_recall(super::RecallOutcome::Conflict);
        let text = super::prometheus_recall_metrics();
        assert!(
            text.contains("aikoql_recall_quality_total{outcome=\"empty\"} 1"),
            "got: {text}"
        );
        assert!(
            text.contains("aikoql_recall_quality_total{outcome=\"conflict\"} 1"),
            "got: {text}"
        );
    }
}
