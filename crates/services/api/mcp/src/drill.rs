//! PROD-7 — `aikoql-mcp restore-drill`: prove the archive restores.
//!
//! A backup that has never been restored is a hope. The drill restores
//! the archive (`archive::restore`, the same code `pitr` runs) into a
//! scratch directory of its own, opens the database there — the engine's
//! recovery and integrity checks run on open — reads its health, and
//! writes one JSON report. Exit 0 only when every check passed, 1 when
//! the drill ran and something failed, 2 on a usage error: a scheduler
//! alerts on the exit code and files the report.
//!
//! Safety: the only directory written is a fresh scratch directory this
//! process creates (it refuses one that already exists) and removes. The
//! archive is only read. A serving database is never opened.

use crate::archive::{self, Until};
use serde_json::{json, Value as J};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const USAGE: &str = "Usage: aikoql-mcp restore-drill --archive DIR|s3://BUCKET[/PREFIX] \
[--until-seq N | --until-ms UNIX_MS] [--expect-min-seq N] [--report FILE] [--scratch-root DIR]";

struct Opts {
    archive: String,
    until: Until,
    expect_min_seq: Option<u64>,
    report: Option<PathBuf>,
    scratch_root: PathBuf,
}

fn parse(tail: &[&str]) -> Option<Opts> {
    let (mut archive, mut until, mut expect, mut report) = (None, Until::Latest, None, None);
    let mut scratch_root = std::env::temp_dir();
    let mut it = tail.iter();
    while let Some(a) = it.next() {
        let value = it.next()?;
        match *a {
            "--archive" => archive = Some(value.to_string()),
            "--until-seq" => until = Until::Seq(value.parse().ok()?),
            "--until-ms" => until = Until::Ms(value.parse().ok()?),
            "--expect-min-seq" => expect = Some(value.parse().ok()?),
            "--report" => report = Some(PathBuf::from(value)),
            "--scratch-root" => scratch_root = PathBuf::from(value),
            _ => return None,
        }
    }
    Some(Opts {
        archive: archive?,
        until,
        expect_min_seq: expect,
        report,
        scratch_root,
    })
}

/// The bytes under `dir` (0 for what cannot be read: a size, not a check).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// One drill's findings, in the order they were established.
#[derive(Default)]
struct Findings {
    checks: Vec<J>,
    restored_seq: u64,
    term: u64,
    bytes_restored: u64,
    journal_seq: u64,
    object_count: u64,
    error: Option<String>,
}

impl Findings {
    fn check(&mut self, name: &str, ok: bool, detail: String) -> bool {
        self.checks
            .push(json!({"name": name, "ok": ok, "detail": detail}));
        if !ok && self.error.is_none() {
            self.error = Some(format!("{name}: {detail}"));
        }
        ok
    }
}

/// Restore into `scratch` and examine the result. Returns early at the
/// first check that makes the later ones meaningless.
fn examine(opts: &Opts, scratch: &Path, found: &mut Findings) {
    let restored = archive::open_store(&opts.archive, Duration::from_secs(60)).and_then(|store| {
        let tmp = PathBuf::from(format!("{}.pitr-tmp", scratch.display()));
        let r = archive::restore(&*store, scratch, opts.until, &tmp);
        if let Err(e) = std::fs::remove_dir_all(&tmp) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %tmp.display(), "restore-drill: scratch residue: {e}");
            }
        }
        r
    });
    match restored {
        Ok((seq, term)) => {
            (found.restored_seq, found.term) = (seq, term);
            found.bytes_restored = dir_bytes(scratch);
            found.check("restore", true, format!("seq {seq}, term {term}"));
        }
        Err(e) => {
            found.check("restore", false, e);
            return;
        }
    }
    if let Some(min) = opts.expect_min_seq {
        let seq = found.restored_seq;
        found.check(
            "expected seq",
            seq >= min,
            format!("restored to seq {seq}, expected at least {min}"),
        );
    }
    // opening runs the engine's recovery: WAL replay, manifest and
    // segment validation — a damaged restore fails here, not in production
    let Some(dir) = scratch.to_str() else {
        found.check("open", false, "the scratch path is not UTF-8".into());
        return;
    };
    let kernel = match crate::engine::open_kernel_auto(dir) {
        Ok((kernel, _admin)) => {
            found.check("open", true, "the restored database opens".into());
            kernel
        }
        Err(e) => {
            found.check("open", false, e.to_string());
            return;
        }
    };
    match crate::tools::admin::tool_health(&kernel) {
        Ok(h) => {
            found.journal_seq = h["journal_seq"].as_u64().unwrap_or(0);
            found.object_count = h["object_count"].as_u64().unwrap_or(0);
            found.check(
                "health",
                h["status"] == json!("healthy"),
                format!(
                    "{}; journal seq {}, {} objects",
                    h["status"].as_str().unwrap_or("no status"),
                    found.journal_seq,
                    found.object_count
                ),
            );
        }
        Err(e) => {
            found.check("health", false, e);
        }
    }
    // `kernel` drops here: the scratch directory is closed before removal
}

/// Run the drill for the arguments after `restore-drill`; the exit code.
pub(crate) fn run(tail: &[&str]) -> i32 {
    let Some(opts) = parse(tail) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let started = Instant::now();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let scratch = opts.scratch_root.join(format!(
        "aikoql-restore-drill-{}-{stamp}",
        std::process::id()
    ));
    let mut found = Findings::default();
    if scratch.exists() {
        found.check(
            "scratch",
            false,
            format!(
                "{} already exists — refusing to write into it",
                scratch.display()
            ),
        );
    } else {
        examine(&opts, &scratch, &mut found);
        if let Err(e) = std::fs::remove_dir_all(&scratch) {
            if e.kind() != std::io::ErrorKind::NotFound {
                found.check("cleanup", false, format!("{}: {e}", scratch.display()));
            }
        }
    }
    let ok = found.error.is_none();
    let report = json!({
        "ok": ok,
        "archive": opts.archive,
        "scratch": scratch.display().to_string(),
        "restored_seq": found.restored_seq,
        "term": found.term,
        "bytes_restored": found.bytes_restored,
        "journal_seq": found.journal_seq,
        "object_count": found.object_count,
        "duration_ms": started.elapsed().as_millis() as u64,
        "finished_unix_ms": stamp as u64 + started.elapsed().as_millis() as u64,
        "checks": found.checks,
        "error": found.error,
    });
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string());
    println!("{text}");
    if let Some(path) = &opts.report {
        if let Err(e) = std::fs::write(path, &text) {
            eprintln!("restore-drill: write the report {}: {e}", path.display());
            return 1;
        }
    }
    i32::from(!ok)
}
