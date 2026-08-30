# Wins (measured)

Knowledge-native execution where the substrate is decisively better than the
mechanical baseline, per the plan's §4 ground-truth-first rule. All numbers
committed in `crates/ingestion/tests/wave5_ka.rs`.

| Workload | AIKOQL (kernel ops) | Mechanical RAG | Margin |
| --- | --- | --- | --- |
| W5-KA-001 multi-hop dependency chain (6 hops) | 6/6 hops, 0 false, 0 missed, 12 app LOC | 2/5 chain chunks + 1 false chunk packed (lexical rank + budget pack) | 6/6 vs 2/5 — traversal follows edges, not token overlap |
| W5-KA-003 provenance-aware risk | full derivation record + swept classification on premise supersede | n/a (chunk pack cannot represent derivation or sweep) | the why is a first-class record, not a paragraph search |
| W5-KA-004 conflicting evidence | effective state BLOCKED, conflict disclosed, loser traceable | n/a (both claim chunks rank equally; nothing records the decision) | conflict resolution is an op, not a prompt |
| W5-KA-008 historical reconstruction | t=500/t=1500/t=2500 exact, sources per era | n/a (a chunk pack mixes eras; nothing pins validity windows) | valid_from/valid_to is queryable state |

Also a TDD dividend: writing KA-001 RED-first exposed a real substrate bug —
the graph-engine traverse fast path collected outbound edges for
`Direction::Inbound` queries and labeled every hit Outbound. Fixed at the
root (`crates/engines/graph/src/lib.rs`); inbound impact walks are now
direction-exact. This is the plan's §28 position working as intended: the
benchmark discovers the boundary, it does not benchmark ClickHouse.
