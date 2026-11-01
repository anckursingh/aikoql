//! AG-40 live queries: `koql/subscribe` — change events when a query's
//! result set changes, built on the kernel's durable CDC subscriptions
//! (subscribe/replay/ack). The dispatcher arm turns each emission into a
//! `notifications/koql/updated` frame; this module is the kernel-side core
//! so the no-missed/no-duplicate gate is testable without a wire.

use crate::tools::query::execute_readonly;
use crate::{json, Kernel, J};
use aikoql_kernel::knowledge::kom::KnowledgeEvent;
use aikoql_kernel::knowledge::notify::EventFilter;
use std::sync::mpsc;

/// A live query: the initial result set plus a channel of
/// `(seq, results)` change emissions, strictly increasing in seq.
#[derive(Debug)]
pub(crate) struct LiveSubscription {
    pub(crate) results: J,
    pub(crate) rx: mpsc::Receiver<(u64, J)>,
}

/// Compile + run `query` once, then subscribe to the commit stream and
/// re-evaluate per event in commit order: an emission fires only when the
/// result set (the JSON rows) changes. The backlog is replayed first from
/// the kernel's persisted per-subscription seq, so a reconnect misses
/// nothing; a boundary event double-evaluated by replay+live is absorbed by
/// the snapshot diff. Every event is acked, so the durable seq advances
/// past processed events.
///
/// shortcut: the query re-runs on every commit (no type-narrowed
/// EventFilter) — narrow it when a hot store shows the cost.
pub(crate) fn live_query_stream(
    k: &Kernel,
    sub_id: &str,
    query: &str,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
) -> Result<LiveSubscription, String> {
    // Read-only guard, like koql/query.
    match aikoql_compiler::parser::parse(query) {
        Ok(aikoql_compiler::parser::ast::Statement::Match(_)) => {}
        Ok(_) => {
            return Err("koql/subscribe is read-only — subscribe to a MATCH query".into());
        }
        Err(e) => return Err(format!("parse error: {e}")),
    }

    let sub_key = format!("koql:{sub_id}");
    // Subscribe BEFORE the initial evaluation: the kernel records
    // last_seq = current_seq at subscribe time, so events committed between
    // an earlier evaluation and the subscribe call would be neither backlog
    // nor live — the hole. In this order, pre-subscribe events are covered
    // by the fresh evaluation and the subscribe→eval window is covered twice
    // (backlog + live), which the snapshot diff absorbs.
    let rx = k
        .subscribe(sub_key.clone(), EventFilter::default())
        .map_err(|e| e.to_string())?;
    let backlog = k.replay(&sub_key).map_err(|e| e.to_string())?;
    let initial = execute_readonly(k, query, subject, roles, tenant)?;

    let (tx, out_rx) = mpsc::channel();
    let initial_for_thread = initial.clone();
    let k = k.clone_handle();
    let query = query.to_string();
    let subject = subject.to_string();
    let roles = roles.to_vec();
    let tenant = tenant.map(String::from);
    let sub_key = sub_key.clone();
    std::thread::spawn(move || {
        let mut snapshot = initial_for_thread;
        let evaluate = |ke: &KnowledgeEvent, snapshot: &mut J| {
            // An evaluation error is emitted as a change — the client sees
            // the failure and a recovery emission when the query heals.
            let results = execute_readonly(&k, &query, &subject, &roles, tenant.as_deref())
                .unwrap_or_else(|e| json!({"error": e}));
            if results != *snapshot {
                let _ = tx.send((ke.seq, results.clone()));
                *snapshot = results;
            }
            let _ = k.ack(&sub_key, ke.seq);
        };
        for ke in &backlog {
            evaluate(ke, &mut snapshot);
        }
        for ke in rx {
            evaluate(&ke, &mut snapshot);
        }
    });
    Ok(LiveSubscription {
        results: initial,
        rx: out_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool_remember;
    use aikoql_kernel::{ManualClock, MemoryEngine};
    use std::sync::Arc;
    use std::time::Duration;

    fn mk() -> Kernel {
        Kernel::open(
            Arc::new(MemoryEngine::new()),
            Arc::new(ManualClock::new(10_000)),
            0xA640,
        )
        .unwrap()
    }

    fn remember(k: &Kernel, tag: &str) {
        tool_remember(
            k,
            &json!({
                "subject": "mcp-agent", "type_name": "LiveThing",
                "properties": {"tag": tag}
            }),
        )
        .expect("remember");
    }

    /// The AG-40 gate: no missed and no duplicate emissions under
    /// concurrent writers. Every create changes the result set, so each
    /// emission must carry strictly more rows than the last (a duplicate
    /// evaluation is absorbed by the snapshot diff), seqs strictly
    /// increase, and the last emission — evaluated after the sentinel
    /// write — must hold the full final set. Durability: every event is
    /// acked, so a replay after quiescence is empty.
    #[test]
    fn ag40_no_missed_no_duplicate_under_concurrent_writers() {
        let k = mk();
        remember(&k, "seed");

        let ls = live_query_stream(
            &k,
            "ag40",
            "MATCH LiveThing RETURN *",
            "mcp-agent",
            &[],
            None,
        )
        .expect("subscribe");
        assert_eq!(
            ls.results.as_array().map(|a| a.len()),
            Some(1),
            "initial result set must be the seed: {:?}",
            ls.results
        );

        // 4 concurrent writers × 5 creates on one kernel.
        let k_arc = Arc::new(k);
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let k = k_arc.clone();
                std::thread::spawn(move || {
                    for i in 0..5 {
                        remember(&k, &format!("w{w}-{i}"));
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        // Sentinel write: its event is the last commit, so the emission
        // evaluated after it must hold the full 1 + 4*5 + 1 = 22 rows.
        remember(&k_arc, "sentinel");

        let mut emissions: Vec<(u64, usize)> = Vec::new();
        loop {
            let (seq, results) = match ls.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(e) => e,
                Err(_) => break,
            };
            let rows = results.as_array().map(|a| a.len()).unwrap_or(0);
            if let Some((last_seq, last_rows)) = emissions.last() {
                assert!(
                    seq > *last_seq,
                    "seqs must strictly increase: {emissions:?}"
                );
                assert!(
                    rows > *last_rows,
                    "a duplicate or stale emission at seq {seq}: {emissions:?} → {rows}"
                );
            }
            emissions.push((seq, rows));
            if rows == 22 {
                break; // the sentinel's evaluation — nothing can follow it
            }
        }
        assert!(
            !emissions.is_empty(),
            "the subscription must emit under concurrent writers"
        );
        assert_eq!(
            emissions.last().map(|(_, r)| *r),
            Some(22),
            "the final emission must hold the full result set: {emissions:?}"
        );
        // Durability: every event is acked (emit-then-ack), so a replay
        // after quiescence is empty — poll briefly, the worker's acks lag
        // its emissions.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !k_arc.replay("koql:ag40").unwrap().is_empty() && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            k_arc.replay("koql:ag40").unwrap().len(),
            0,
            "all processed events must be acked (no missed replay backlog)"
        );
        // Nothing follows the final emission.
        assert!(
            ls.rx.try_recv().is_err(),
            "no emission may follow the final state"
        );
    }

    /// Live queries are read-only: a write statement is rejected at
    /// subscribe time, like koql/query.
    #[test]
    fn ag40_subscribe_rejects_writes() {
        let k = mk();
        let err = live_query_stream(
            &k,
            "ag40w",
            "CREATE OtherThing {x: 1}",
            "mcp-agent",
            &[],
            None,
        )
        .unwrap_err();
        assert!(err.contains("read-only"), "got: {err}");
    }
}
