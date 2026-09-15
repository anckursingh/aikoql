//! P5-M0 — plan-equivalence oracle (gate 6).
//!
//! Executes a baseline and a candidate plan against the same kernel and
//! compares deterministic row-set fingerprints. Divergence 0 = equivalent by
//! observation. Any execution error on either side is itself a divergence —
//! the oracle never reports equivalence through a failure.

use super::{Interpreter, RowSet};
use aikoql_kernel::ir::{IrPlan, PhysicalPlan};
use aikoql_kernel::transaction::kernel::Kernel;

/// One baseline/candidate plan pair to compare.
pub struct OracleEntry {
    pub id: String,
    pub baseline: IrPlan,
    pub candidate: IrPlan,
}

#[derive(Debug)]
pub struct OracleReport {
    pub entries: usize,
    pub divergences: Vec<String>,
}

/// Run every entry against `kernel`. Equivalent plans (same fingerprint) are
/// silent; every divergence becomes one human-readable line.
pub fn run(kernel: &Kernel, entries: &[OracleEntry]) -> OracleReport {
    run_impl(kernel, entries, false)
}

/// P5-M9 (ND-08, gate 6): the baseline executes as the RULE physicalization
/// (the pre-M15 default path) and the candidate through the default
/// cost-optimized path (`Interpreter::execute` — P5-M15 made the CBO the
/// default) — the pin that index-assisted plans never change results.
pub fn run_costed(kernel: &Kernel, entries: &[OracleEntry]) -> OracleReport {
    run_impl(kernel, entries, true)
}

fn run_impl(kernel: &Kernel, entries: &[OracleEntry], costed: bool) -> OracleReport {
    let mut divergences = Vec::new();
    for e in entries {
        let baseline = if costed {
            Interpreter::execute_physical(
                kernel,
                &PhysicalPlan::from_ops(e.baseline.operators.clone()),
            )
        } else {
            Interpreter::execute(kernel, &e.baseline)
        };
        let candidate = Interpreter::execute(kernel, &e.candidate);
        match (baseline, candidate) {
            (Ok(b), Ok(c)) => {
                let (fb, fc) = (fingerprint(&b), fingerprint(&c));
                if fb != fc {
                    divergences.push(format!("{}: baseline {} != candidate {}", e.id, fb, fc));
                }
            }
            (Err(be), Ok(_)) => divergences.push(format!(
                "{}: baseline errored ({be}) but candidate succeeded",
                e.id
            )),
            (Ok(_), Err(ce)) => divergences.push(format!(
                "{}: candidate errored ({ce}) but baseline succeeded",
                e.id
            )),
            (Err(be), Err(ce)) => divergences.push(format!(
                "{}: both sides errored — baseline ({be}), candidate ({ce})",
                e.id
            )),
        }
    }
    OracleReport {
        entries: entries.len(),
        divergences,
    }
}

/// Deterministic per-row key: koid hex + version. KOs from the same kernel
/// snapshot are identical objects, so koid+version is the full identity.
fn fingerprint(rows: &RowSet) -> String {
    match rows {
        RowSet::Objects(objs) => {
            let mut keys: Vec<String> = objs
                .iter()
                .map(|o| format!("o:{:x?}:{}", o.koid.0, o.version))
                .collect();
            keys.sort();
            format!("Objects[{}]:{}", keys.len(), keys.join("|"))
        }
        RowSet::Scored(items) => {
            let mut keys: Vec<String> = items
                .iter()
                .map(|(k, s, t, _)| format!("s:{:x?}:{:?}:{t}", k.0, s.to_bits()))
                .collect();
            keys.sort();
            format!("Scored[{}]:{}", keys.len(), keys.join("|"))
        }
        RowSet::Traversal(items) => {
            let mut keys: Vec<String> = items
                .iter()
                .map(|(k, r, d)| format!("t:{:x?}:{r}:{d}", k.0))
                .collect();
            keys.sort();
            format!("Traversal[{}]:{}", keys.len(), keys.join("|"))
        }
        RowSet::Grouped(groups) => {
            let mut keys: Vec<String> = groups
                .iter()
                .map(|m| {
                    let mut kv: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                    kv.sort();
                    kv.join("&")
                })
                .collect();
            keys.sort();
            format!("Grouped[{}]:{}", keys.len(), keys.join("|"))
        }
        // P5-M6 (ND-06): a pair is (left koid+version, right koid+version |
        // None) — the identity of both sides, not their property content.
        RowSet::Joined(pairs) => {
            let mut keys: Vec<String> = pairs
                .iter()
                .map(|(l, r)| {
                    format!(
                        "j:{:x?}:{}:{}",
                        l.koid.0,
                        l.version,
                        r.as_ref()
                            .map(|ro| format!("{:x?}:{}", ro.koid.0, ro.version))
                            .unwrap_or_else(|| "none".into())
                    )
                })
                .collect();
            keys.sort();
            format!("Joined[{}]:{}", keys.len(), keys.join("|"))
        }
    }
}
