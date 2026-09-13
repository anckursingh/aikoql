//! P5-M0 — plan-equivalence oracle (gate 6).
//!
//! Executes a baseline and a candidate plan against the same kernel and
//! compares deterministic row-set fingerprints. Divergence 0 = equivalent by
//! observation. Any execution error on either side is itself a divergence —
//! the oracle never reports equivalence through a failure.

use super::{Interpreter, RowSet};
use aikoql_kernel::ir::IrPlan;
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
    let mut divergences = Vec::new();
    for e in entries {
        match (
            Interpreter::execute(kernel, &e.baseline),
            Interpreter::execute(kernel, &e.candidate),
        ) {
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
    }
}
