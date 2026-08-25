//! MVP-QA-002 Suite C — adversarial retrieval (QA2-RET-008, graph leg).
//!
//! RET-003/005 (context compiler) live in
//! aikoql-ingestion/tests/qa2_retrieval.rs. RET-008 is the multi-hop pin:
//! A → depends_on → B → depends_on → C → owned_by → Team X →
//! located_in → Amsterdam. The complete reasoning chain must be available
//! within the configured traversal limit, and the limit must hold — a
//! deeper configured depth never leaks beyond it, a shallower one cleanly
//! truncates (chain prefix, no partial-hybrid hits).

use aikoql_kernel::*;
use std::collections::HashSet;
use std::sync::Arc;

fn mk() -> (Kernel, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), 0xC0FFEE).unwrap();
    (k, clock)
}

fn meta(t: &str) -> Metadata {
    Metadata {
        type_name: t.into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn alice() -> Subject {
    Subject::new("alice")
}

fn node(k: &Kernel, name: &str, rels: Vec<(&str, KOID)>) -> KOID {
    let mut r = RememberRequest::create(alice(), meta("node"));
    r.properties.insert("name".into(), Value::Text(name.into()));
    for (rel_type, target) in rels {
        r.relationships.push(RelationshipRef {
            rel_type: rel_type.into(),
            target,
            direction: Direction::Outbound,
        });
    }
    k.remember(r).unwrap().koid
}

#[test]
fn w2_ret_008_deep_multihop_chain_available_within_configured_limits() {
    let (k, _c) = mk();
    let amsterdam = node(&k, "Amsterdam", vec![]);
    let team_x = node(&k, "Team X", vec![("located_in", amsterdam)]);
    let c = node(&k, "C", vec![("owned_by", team_x)]);
    let b = node(&k, "B", vec![("depends_on", c)]);
    let a = node(&k, "A", vec![("depends_on", b)]);

    // depth 4: the complete reasoning chain A→B→C→Team X→Amsterdam is
    // available within the configured limit — every hop present, none
    // skipped or truncated mid-chain.
    let hits = aikoql_graph::GraphEngine::traverse(
        &k,
        aikoql_graph::TraverseQuery::new(alice(), a)
            .with_depth(4)
            .with_direction(Direction::Outbound),
    )
    .unwrap();
    let reached: HashSet<(KOID, u32)> = hits.iter().map(|h| (h.koid, h.depth)).collect();
    for (id, depth) in [(b, 1), (c, 2), (team_x, 3), (amsterdam, 4)] {
        assert!(
            reached.contains(&(id, depth)),
            "chain node at depth {depth} must be reachable within the limit"
        );
    }
    assert!(
        hits.iter().all(|h| h.depth <= 4),
        "no hit may exceed the configured depth"
    );

    // depth 2: the limit truncates cleanly to the chain prefix — B and C
    // only, never a hybrid where a deep node leaks past the limit.
    let hits2 = aikoql_graph::GraphEngine::traverse(
        &k,
        aikoql_graph::TraverseQuery::new(alice(), a)
            .with_depth(2)
            .with_direction(Direction::Outbound),
    )
    .unwrap();
    assert!(hits2.iter().all(|h| h.depth <= 2));
    let reached2: HashSet<KOID> = hits2.iter().map(|h| h.koid).collect();
    assert_eq!(
        reached2,
        HashSet::from([b, c]),
        "depth-2 traversal returns exactly the reachable prefix"
    );

    // The rel-type filter composes with depth: the depends_on-only chain
    // from A ends at C — owned_by/located_in are not smuggled in.
    let hits3 = aikoql_graph::GraphEngine::traverse(
        &k,
        aikoql_graph::TraverseQuery::new(alice(), a)
            .with_rel_type("depends_on")
            .with_depth(4)
            .with_direction(Direction::Outbound),
    )
    .unwrap();
    let reached3: HashSet<KOID> = hits3.iter().map(|h| h.koid).collect();
    assert_eq!(
        reached3,
        HashSet::from([b, c]),
        "rel-type filtering must not leak other predicates into the chain"
    );
}
