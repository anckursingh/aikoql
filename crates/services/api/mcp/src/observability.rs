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

/// A persistent, size-rotated log file. Every line the server logs also lands here, so what
/// happened is still on disk after a restart, a crash or a closed terminal.
#[derive(Clone)]
pub(crate) struct LogFile(std::sync::Arc<std::sync::Mutex<LogInner>>);

struct LogInner {
    path: std::path::PathBuf,
    file: std::fs::File,
    size: u64,
    max: u64,
    keep: usize,
}

impl LogFile {
    /// Open (creating the directory and file) for appending; an existing file is kept.
    pub(crate) fn open(path: &std::path::Path, max: u64, keep: usize) -> std::io::Result<LogFile> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let size = file.metadata()?.len();
        Ok(LogFile(std::sync::Arc::new(std::sync::Mutex::new(
            LogInner {
                path: path.to_path_buf(),
                file,
                size,
                max,
                keep,
            },
        ))))
    }

    /// Append one chunk of log text. A failing disk never takes the server down with it.
    pub(crate) fn write_line(&self, bytes: &[u8]) {
        use std::io::Write;
        let Ok(mut g) = self.0.lock() else { return };
        if g.size > 0 && g.size + bytes.len() as u64 > g.max {
            g.rotate();
        }
        if g.file.write_all(bytes).is_ok() {
            g.size += bytes.len() as u64;
        }
    }
}

impl LogInner {
    /// `log` -> `log.1` -> ... -> `log.<keep>` (the oldest is dropped), then a fresh `log`.
    fn rotate(&mut self) {
        let name = |n: usize| std::path::PathBuf::from(format!("{}.{n}", self.path.display()));
        let _ = std::fs::remove_file(name(self.keep));
        for n in (1..self.keep).rev() {
            let _ = std::fs::rename(name(n), name(n + 1));
        }
        let _ = std::fs::rename(&self.path, name(1));
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = f;
            self.size = 0;
        }
    }
}

/// Terminal colour sequences are for people at a terminal, not for a file.
fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if bytes.get(i) == Some(&b'[') {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
            }
            i += 1; // the final byte of the sequence
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Where the server log goes: `[logging] file` / `AIKOQL_LOG_FILE`; by default next to the
/// database (`<db>.server.log`); `off` (or empty) turns the file off and leaves stderr only.
pub(crate) fn log_file_path(configured: Option<&str>, db_path: &str) -> Option<String> {
    match configured {
        Some("") | Some("off") => None,
        Some(p) => Some(p.to_string()),
        None => Some(format!("{db_path}.server.log")),
    }
}

/// stderr plus the log file, one writer per event (what `tracing_subscriber` asks for).
#[derive(Clone)]
struct Tee(Option<LogFile>);

struct TeeWriter(Option<LogFile>);

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(f) = &self.0 {
            f.write_line(&strip_ansi(buf));
        }
        std::io::stderr().write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tee {
    type Writer = TeeWriter;
    fn make_writer(&'a self) -> TeeWriter {
        TeeWriter(self.0.clone())
    }
}

/// Build the subscriber: env filter + fmt (plain or JSON) + an OTLP layer
/// when `AIKOQL_OTEL_ENDPOINT` is set. The OTLP layer goes first — its
/// Layer impl is typed to the subscriber it sits on, unlike fmt/EnvFilter.
pub(crate) fn init_tracing(log_level: &str, json: bool, ansi: bool, log_file: Option<&str>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));
    const MAX_LOG_BYTES: u64 = 50 * 1024 * 1024;
    const KEEP_LOGS: usize = 5;
    let file = log_file.and_then(|p| {
        LogFile::open(std::path::Path::new(p), MAX_LOG_BYTES, KEEP_LOGS)
            .map_err(|e| eprintln!("cannot open the log file {p}: {e} — logging to stderr only"))
            .ok()
    });
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(Tee(file.clone()))
        .with_ansi(ansi);
    if file.is_some() {
        // a crash must be in the log too, not only on a terminal nobody is watching
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!(panic = %info, "the server panicked");
            previous(info);
        }));
    }
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
    use super::{log_file_path, strip_ansi, LogFile};
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

    // ---- the persistent log file ----

    fn tmp_log(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aikoql-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("nested").join("server.log")
    }

    #[test]
    fn the_log_file_is_created_and_survives_a_restart() {
        let path = tmp_log("restart");
        let log = LogFile::open(&path, 1_000_000, 3).unwrap(); // creates the missing directories
        log.write_line(b"first run\n");
        drop(log);
        let log = LogFile::open(&path, 1_000_000, 3).unwrap(); // a restart appends, never truncates
        log.write_line(b"second run\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first run\nsecond run\n"
        );
    }

    #[test]
    fn the_log_rotates_by_size_and_keeps_a_bounded_history() {
        let path = tmp_log("rotate");
        let log = LogFile::open(&path, 100, 2).unwrap();
        for i in 0..40 {
            log.write_line(format!("line {i:03} {}\n", "x".repeat(20)).as_bytes());
        }
        let name = |n: &str| std::path::PathBuf::from(format!("{}{n}", path.display()));
        assert!(
            path.exists() && name(".1").exists() && name(".2").exists(),
            "current + 2 old files"
        );
        assert!(!name(".3").exists(), "history is bounded to 2");
        for f in [path.clone(), name(".1"), name(".2")] {
            assert!(
                std::fs::metadata(&f).unwrap().len() <= 100 + 40,
                "{f:?} stays near the cap"
            );
        }
        // the newest lines are in the live file
        assert!(std::fs::read_to_string(&path).unwrap().contains("line 039"));
    }

    #[test]
    fn colour_codes_stay_out_of_the_file() {
        assert_eq!(
            strip_ansi(b"\x1b[2m2026\x1b[0m \x1b[32m INFO\x1b[0m hello"),
            b"2026  INFO hello"
        );
        assert_eq!(
            strip_ansi("plain — ünïcode".as_bytes()),
            "plain — ünïcode".as_bytes()
        );
        assert_eq!(strip_ansi(b"cut \x1b[3"), b"cut ");
    }

    #[test]
    fn the_log_path_follows_the_database_unless_configured() {
        assert_eq!(
            log_file_path(None, "/data/kb"),
            Some("/data/kb.server.log".into())
        );
        assert_eq!(
            log_file_path(Some("/var/log/aikoql.log"), "/data/kb"),
            Some("/var/log/aikoql.log".into())
        );
        assert_eq!(log_file_path(Some("off"), "/data/kb"), None);
        assert_eq!(log_file_path(Some(""), "/data/kb"), None);
    }
}
