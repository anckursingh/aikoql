//! AG-50 observability: OTLP trace export, per-tool call metrics and the
//! recall quality counter.
//!
//! - `record_tool_call` / `prometheus_tool_metrics` — one row per tool on
//!   the Prometheus scrape (calls, errors, latency sum), fed by the
//!   dispatcher's tools/call arm.
//! - `record_recall` / `prometheus_recall_metrics` — the recall quality
//!   counter (empty results, conflict hits).
//! - `init_tracing` — the subscriber: fmt (plain/json) plus an OTLP layer
//!   when `AIKOQL_OTEL_ENDPOINT` is set (e.g. `http://collector:4318`).
//!   The tool spans come from the dispatcher (`tool_call`) and the kernel
//!   ops from the tool bodies (`remember`, `aikoql.execute`, …); the gate
//!   (a trace for every tool call) is pinned by tests/observability.rs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Default, Clone, Copy)]
pub(crate) struct ToolStats {
    pub(crate) calls: u64,
    pub(crate) errors: u64,
    pub(crate) latency_ns: u64,
}

static TOOL_STATS: OnceLock<Mutex<HashMap<String, ToolStats>>> = OnceLock::new();

pub(crate) fn record_tool_call(tool: &str, err: bool, latency: Duration) {
    // justified: Mutex poison is unrecoverable
    let mut map = TOOL_STATS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let s = map.entry(tool.to_string()).or_default();
    s.calls += 1;
    if err {
        s.errors += 1;
    }
    s.latency_ns += latency.as_nanos() as u64;
}

pub(crate) fn tool_stats() -> HashMap<String, ToolStats> {
    TOOL_STATS
        .get()
        .map(|m| m.lock().unwrap().clone()) // justified: Mutex poison is unrecoverable
        .unwrap_or_default()
}

/// Prometheus lines for the per-tool counters: one family each for calls,
/// errors and latency (seconds, summed), labeled by tool.
pub(crate) fn prometheus_tool_metrics() -> String {
    let stats = tool_stats();
    let mut tools: Vec<_> = stats.keys().cloned().collect();
    tools.sort();
    let mut out = String::from(
        "# HELP aikoql_tool_calls_total Tool calls, labeled by tool.\n\
         # TYPE aikoql_tool_calls_total counter\n",
    );
    for t in &tools {
        out.push_str(&format!(
            "aikoql_tool_calls_total{{tool=\"{t}\"}} {}\n",
            stats[t].calls
        ));
    }
    out.push_str(
        "# HELP aikoql_tool_errors_total Tool call errors, labeled by tool.\n\
         # TYPE aikoql_tool_errors_total counter\n",
    );
    for t in &tools {
        out.push_str(&format!(
            "aikoql_tool_errors_total{{tool=\"{t}\"}} {}\n",
            stats[t].errors
        ));
    }
    out.push_str(
        "# HELP aikoql_tool_latency_seconds_total Tool call latency, summed (seconds).\n\
         # TYPE aikoql_tool_latency_seconds_total counter\n",
    );
    for t in &tools {
        out.push_str(&format!(
            "aikoql_tool_latency_seconds_total{{tool=\"{t}\"}} {:.6}\n",
            stats[t].latency_ns as f64 / 1e9
        ));
    }
    out
}

/// What made a recall worth counting (AG-50): it came back with no items,
/// or it surfaced open conflicts.
pub(crate) enum RecallOutcome {
    Empty,
    Conflict,
}

static RECALL_EMPTY: AtomicU64 = AtomicU64::new(0);
static RECALL_CONFLICT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn record_recall(outcome: RecallOutcome) {
    match outcome {
        RecallOutcome::Empty => {
            RECALL_EMPTY.fetch_add(1, Ordering::Relaxed);
        }
        RecallOutcome::Conflict => {
            RECALL_CONFLICT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Both lines render from the start (0 until the first occurrence), so a
/// scrape can alert on the absence of the metric family itself.
pub(crate) fn prometheus_recall_metrics() -> String {
    format!(
        "# HELP aikoql_recall_quality_total Recall quality outcomes, labeled by outcome.\n\
         # TYPE aikoql_recall_quality_total counter\n\
         aikoql_recall_quality_total{{outcome=\"empty\"}} {}\n\
         aikoql_recall_quality_total{{outcome=\"conflict\"}} {}\n",
        RECALL_EMPTY.load(Ordering::Relaxed),
        RECALL_CONFLICT.load(Ordering::Relaxed)
    )
}

/// Build the subscriber: env filter + fmt (plain or JSON) + an OTLP layer
/// when `AIKOQL_OTEL_ENDPOINT` is set. The OTLP layer goes first — its
/// Layer impl is typed to the subscriber it sits on, unlike fmt/EnvFilter.
pub(crate) fn init_tracing(log_level: &str, json: bool, ansi: bool) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(ansi);
    // fmt's subscriber type is baked in at construction, so it goes on first;
    // the OTel layer's subscriber type is a parameter, so it goes on last
    // (each arm calls otel_layer() so S matches that arm's fmt type).
    match json {
        false => match otel_layer() {
            None => tracing_subscriber::registry()
                .with(fmt)
                .with(env_filter)
                .init(),
            Some(otel) => tracing_subscriber::registry()
                .with(fmt)
                .with(env_filter)
                .with(otel)
                .init(),
        },
        true => match otel_layer() {
            None => tracing_subscriber::registry()
                .with(fmt.json())
                .with(env_filter)
                .init(),
            Some(otel) => tracing_subscriber::registry()
                .with(fmt.json())
                .with(env_filter)
                .with(otel)
                .init(),
        },
    }
}

/// The OTLP layer, when the endpoint env is set. The provider is leaked —
/// it must outlive every span, and a server process ends by exit anyway
/// (shortcut: no flush-on-shutdown; the batch exporter's 5 s schedule
/// covers it, and the gate test polls the collector).
fn otel_layer<S>(
) -> Option<tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    let endpoint = std::env::var("AIKOQL_OTEL_ENDPOINT").ok()?;
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .inspect_err(|e| eprintln!("otel: exporter setup failed: {e}"))
        .ok()?;
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();
    let tracer = provider.tracer("aikoql");
    Box::leak(Box::new(provider));
    Some(tracing_opentelemetry::layer().with_tracer(tracer))
}

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
