//! P3-M7 cb004 harness — submits a PARKED reason job, reports once the
//! Running record is durable, then idles until the parent hard-kills it.
//!
//! The park is 60s of the worker's 120s idle: the parent kills mid-job,
//! reopens, and asserts the job reopens as Failed("interrupted") — never
//! silently dropped.

use aikoql_kernel::*;
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("db path");
    let progress = args.next().expect("progress file");

    let engine = RedbEngine::open(&path).expect("open store");
    let k = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 7).expect("open kernel");

    // The worker parks 60s before running — the parent kills us inside it.
    k.set_job_park_ms(60_000);

    let job = k
        .reason(
            "sensor",
            [("zone".to_string(), Value::Text("a".into()))]
                .into_iter()
                .collect(),
        )
        .expect("submit job");

    // The Running record was written before submit returned, so once this
    // file exists the parent may kill us and still observe the job.
    std::fs::write(&progress, format!("running {}", job.job_id)).expect("report");

    std::thread::sleep(std::time::Duration::from_secs(120));
}
