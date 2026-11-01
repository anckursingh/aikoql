//! AG-40 live queries: `koql/subscribe` — change events when a query's
//! result set changes, built on the kernel's durable CDC subscriptions
//! (subscribe/replay/ack). The dispatcher arm turns each emission into a
//! `notifications/koql/updated` frame; this module is the kernel-side core
//! so the no-missed/no-duplicate gate is testable without a wire.

// The tests below are the AG-40 RED: they pin the API this module must
// grow (live_query_stream + LiveSubscription). The dispatcher arm and the
// wire test in tests/live_query.rs pin the protocol surface.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool_remember;
    use crate::{json, Kernel};
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
                assert!(seq > *last_seq, "seqs must strictly increase: {emissions:?}");
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
        // Durability: every event was acked, so a replay now is empty.
        assert_eq!(
            k_arc.replay("ag40").unwrap().len(),
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
