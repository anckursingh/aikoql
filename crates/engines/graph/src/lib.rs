//! Aikoql Graph Engine.
//!
//! Relationship edge mutation and traversal. The engine is stateless and routes
//! every storage access through the public `aikoql_kernel::Kernel` API; it
//! never touches `StorageEngine` or `KnowledgeRepository` directly (HLD §6,
//! review R3).

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{Kernel, KnowledgeContext, RememberRequest};
use std::collections::{HashSet, VecDeque};

pub use aikoql_kernel::knowledge::kom::Direction;

// ---------------------------------------------------------------------------
// Requests & results
// ---------------------------------------------------------------------------

/// Request to add a directed relationship edge `from -> to` of type `rel_type`.
#[derive(Clone, Debug)]
pub struct RelateRequest {
    pub context: KnowledgeContext,
    pub from: KOID,
    pub to: KOID,
    pub rel_type: String,
}

impl RelateRequest {
    pub fn new(
        context: impl Into<KnowledgeContext>,
        from: KOID,
        to: KOID,
        rel_type: impl Into<String>,
    ) -> Self {
        Self {
            context: context.into(),
            from,
            to,
            rel_type: rel_type.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Related {
    pub koid: KOID,
    pub version: u64,
    pub commit_ts: u64,
}

/// Graph-traversal query.
///
/// Walks relationship edges starting at `start` up to `depth` hops. Targets the
/// caller cannot read at the pinned snapshot (or head) are silently skipped.
#[derive(Clone, Debug)]
pub struct TraverseQuery {
    pub context: KnowledgeContext,
    pub start: KOID,
    pub rel_type: Option<String>,
    pub direction: Option<Direction>,
    pub depth: usize,
}

impl TraverseQuery {
    pub fn new(context: impl Into<KnowledgeContext>, start: KOID) -> Self {
        Self {
            context: context.into(),
            start,
            rel_type: None,
            direction: None,
            depth: 1,
        }
    }

    pub fn with_rel_type(mut self, rel_type: impl Into<String>) -> Self {
        self.rel_type = Some(rel_type.into());
        self
    }

    pub fn with_direction(mut self, direction: Direction) -> Self {
        self.direction = Some(direction);
        self
    }

    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }
}

/// One reachable KO discovered by graph traversal.
#[derive(Clone, Debug)]
pub struct TraverseHit {
    pub koid: KOID,
    pub depth: u32,
    pub rel_type: String,
    pub direction: Direction,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Stateless graph engine.
pub struct GraphEngine;

impl GraphEngine {
    /// Add a directed relationship edge between two existing KOs.
    ///
    /// The caller must have `Write` access on `from` and `Read` access on `to`.
    /// Duplicate identical edges are idempotent.
    pub fn relate(kernel: &Kernel, req: RelateRequest) -> KResult<Related> {
        let ctx = req.context;
        let from = kernel.get(ctx.clone(), &req.from)?;
        kernel.verify(ctx.clone(), &req.from, Action::Write)?;
        if req.rel_type.trim().is_empty() {
            return Err(KError::InvalidObject("rel_type must be non-empty".into()));
        }
        // Target must exist and be readable. Map any error to NotFound so the
        // caller cannot distinguish missing objects from inaccessible ones.
        kernel
            .get(ctx.clone(), &req.to)
            .map_err(|_| KError::NotFound(req.to))?;

        if from.relationships.iter().any(|r| {
            r.rel_type == req.rel_type && r.target == req.to && r.direction == Direction::Outbound
        }) {
            return Ok(Related {
                koid: req.from,
                version: from.version,
                commit_ts: from.commit_ts,
            });
        }

        let mut relationships = from.relationships.clone();
        relationships.push(RelationshipRef {
            rel_type: req.rel_type,
            target: req.to,
            direction: Direction::Outbound,
        });
        let remembered = kernel.remember(remember_request_from_object(ctx, from, relationships))?;
        Ok(Related {
            koid: remembered.koid,
            version: remembered.version,
            commit_ts: remembered.commit_ts,
        })
    }

    /// Traverse relationship edges from `start` up to `depth` hops.
    ///
    /// Uses the relationship index for O(edges) scans instead of loading full
    /// KOs. Falls back to KO-loaded traversal for snapshot (time-travel) queries
    /// since the index is not versioned.
    ///
    /// Only returns KOIDs the caller can read. Cycles and inaccessible targets
    /// are silently skipped.
    pub fn traverse(kernel: &Kernel, q: TraverseQuery) -> KResult<Vec<TraverseHit>> {
        let ctx = q.context.clone();

        // Snapshot reads fall back to KO-loaded traversal — the relationship
        // index only reflects current heads, not historical state.
        if ctx.snapshot.is_some() {
            return Self::traverse_from_ko(kernel, &ctx, &q);
        }

        // Fast path: verify start node exists + is readable (one KO load).
        if kernel.get(ctx.clone(), &q.start).is_err() {
            return Err(KError::NotFound(q.start));
        }
        if kernel.verify(ctx.clone(), &q.start, Action::Read).is_err() {
            return Err(KError::NotFound(q.start));
        }
        if q.depth == 0 {
            return Ok(Vec::new());
        }

        // Fast path: index-only BFS.
        let mut hits = Vec::new();
        let mut visited: HashSet<KOID> = HashSet::new();
        let mut queue: VecDeque<(KOID, u32)> = VecDeque::new();
        visited.insert(q.start);
        queue.push_back((q.start, 0));

        while let Some((cur, depth)) = queue.pop_front() {
            if depth >= q.depth as u32 {
                continue;
            }
            let mut edges: Vec<(String, KOID)> = kernel
                .outbound_edges(&cur, q.rel_type.as_deref())?;
            // Merge inbound edges when direction is None (both) or Inbound.
            if q.direction != Some(Direction::Outbound) {
                let inbound = kernel
                    .inbound_edges(&cur, q.rel_type.as_deref())?;
                edges.extend(inbound);
            }
            for (rel_type, target) in edges {
                let next_depth = depth + 1;
                if kernel.verify(ctx.clone(), &target, Action::Read).is_err() {
                    continue;
                }
                if visited.insert(target) {
                    hits.push(TraverseHit {
                        koid: target,
                        depth: next_depth,
                        rel_type: rel_type.clone(),
                        direction: Direction::Outbound,
                    });
                    queue.push_back((target, next_depth));
                }
            }
        }
        Ok(hits)
    }

    /// Fallback: KO-loaded BFS for snapshot/time-travel queries where the
    /// relationship index (which only tracks current heads) is stale.
    fn traverse_from_ko(
        kernel: &Kernel,
        ctx: &KnowledgeContext,
        q: &TraverseQuery,
    ) -> KResult<Vec<TraverseHit>> {
        let resolve = |koid: &KOID| -> Option<KnowledgeObject> {
            match ctx.snapshot {
                Some(ts) => kernel.get_at(ctx.clone(), koid, ts).ok(),
                None => kernel.get(ctx.clone(), koid).ok(),
            }
        };

        // Verify start node.
        resolve(&q.start).ok_or(KError::NotFound(q.start))?;
        if kernel.verify(ctx.clone(), &q.start, Action::Read).is_err() {
            return Err(KError::NotFound(q.start));
        }
        if q.depth == 0 {
            return Ok(Vec::new());
        }

        let mut hits = Vec::new();
        let mut visited: HashSet<KOID> = HashSet::new();
        let mut queue: VecDeque<(KOID, u32)> = VecDeque::new();
        visited.insert(q.start);
        queue.push_back((q.start, 0));

        while let Some((cur, depth)) = queue.pop_front() {
            if depth >= q.depth as u32 {
                continue;
            }
            let Some(ko) = resolve(&cur) else {
                continue;
            };
            for rel in &ko.relationships {
                if let Some(ref rt) = q.rel_type {
                    if &rel.rel_type != rt {
                        continue;
                    }
                }
                if let Some(dir) = q.direction {
                    if rel.direction != dir {
                        continue;
                    }
                }
                let next_depth = depth + 1;
                let Some(target) = resolve(&rel.target) else {
                    continue;
                };
                if kernel
                    .verify(ctx.clone(), &target.koid, Action::Read)
                    .is_err()
                {
                    continue;
                }
                if visited.insert(rel.target) {
                    hits.push(TraverseHit {
                        koid: rel.target,
                        depth: next_depth,
                        rel_type: rel.rel_type.clone(),
                        direction: rel.direction,
                    });
                    queue.push_back((rel.target, next_depth));
                }
            }
        }
        Ok(hits)
    }
}

/// Ergonomic extension trait so callers can write `kernel.relate(...)` after
/// importing `GraphEngineApi`.
pub trait GraphEngineApi {
    fn relate(&self, req: RelateRequest) -> KResult<Related>;
    fn traverse(&self, q: TraverseQuery) -> KResult<Vec<TraverseHit>>;
}

impl GraphEngineApi for Kernel {
    fn relate(&self, req: RelateRequest) -> KResult<Related> {
        GraphEngine::relate(self, req)
    }

    fn traverse(&self, q: TraverseQuery) -> KResult<Vec<TraverseHit>> {
        GraphEngine::traverse(self, q)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn remember_request_from_object(
    ctx: KnowledgeContext,
    ko: KnowledgeObject,
    relationships: Vec<RelationshipRef>,
) -> RememberRequest {
    RememberRequest {
        context: ctx,
        koid: Some(ko.koid),
        expected_version: None,
        idempotency_key: None,
        metadata: ko.metadata,
        properties: ko.properties,
        semantic: ko.semantic,
        relationships,
        security: None,
        extensions: ko.extensions,
        origin: ko.lifecycle.origin,
        note: None,
        referential_policy: ReferentialPolicy::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::{ManualClock, MemoryEngine, Metadata, RememberRequest, Subject};
    use std::sync::Arc;

    fn meta(t: &str) -> Metadata {
        Metadata {
            type_name: t.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        }
    }

    fn subject(name: &str) -> Subject {
        Subject::new(name)
    }

    fn mk() -> Kernel {
        let clock = Arc::new(ManualClock::new(10_000));
        Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap()
    }

    fn create(k: &Kernel, s: &Subject, t: &str) -> KOID {
        k.remember(RememberRequest::create(s.clone(), meta(t)))
            .unwrap()
            .koid
    }

    #[test]
    fn relate_adds_edge_and_versions_source() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");

        let r = GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();
        assert_eq!(r.koid, n1);
        assert_eq!(r.version, 2);

        let ko = k.get(&a, &n1).unwrap();
        assert_eq!(ko.relationships.len(), 1);
        assert_eq!(ko.relationships[0].target, n2);
        assert_eq!(ko.relationships[0].rel_type, "references");
        assert_eq!(ko.relationships[0].direction, Direction::Outbound);
    }

    #[test]
    fn relate_is_idempotent() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");

        let r1 = GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();
        let r2 = GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();
        assert_eq!(r1.version, r2.version);

        let ko = k.get(&a, &n1).unwrap();
        assert_eq!(ko.relationships.len(), 1);
    }

    #[test]
    fn relate_requires_existing_target() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let missing = KOID::ZERO;

        let err =
            GraphEngine::relate(&k, RelateRequest::new(&a, n1, missing, "references")).unwrap_err();
        assert!(matches!(err, KError::NotFound(_)));
    }

    #[test]
    fn traverse_one_hop() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");
        let n3 = create(&k, &a, "note");
        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();
        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n3, "cites")).unwrap();

        let hits = GraphEngine::traverse(&k, TraverseQuery::new(&a, n1).with_depth(1)).unwrap();
        assert_eq!(hits.len(), 2);
        let mut ids: Vec<KOID> = hits.iter().map(|h| h.koid).collect();
        ids.sort();
        let mut expected = vec![n2, n3];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn traverse_depth_limited() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");
        let n3 = create(&k, &a, "note");
        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "next")).unwrap();
        GraphEngine::relate(&k, RelateRequest::new(&a, n2, n3, "next")).unwrap();

        let depth1 = GraphEngine::traverse(
            &k,
            TraverseQuery::new(&a, n1)
                .with_depth(1)
                .with_rel_type("next"),
        )
        .unwrap();
        assert_eq!(depth1.len(), 1);
        assert_eq!(depth1[0].koid, n2);

        let depth2 = GraphEngine::traverse(
            &k,
            TraverseQuery::new(&a, n1)
                .with_depth(2)
                .with_rel_type("next"),
        )
        .unwrap();
        assert_eq!(depth2.len(), 2);
    }

    #[test]
    fn traverse_skips_inaccessible_targets() {
        let k = mk();
        let alice = subject("alice");
        let bob = subject("bob");

        let mut n1 = RememberRequest::create(alice.clone(), meta("note"));
        n1.security = Some(SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![AclEntry {
                principal: "bob".into(),
                action: Action::Read,
                effect: Effect::Allow,
            }],
            classification: None,
        });
        let n1 = k.remember(n1).unwrap().koid;

        let mut n2 = RememberRequest::create(alice.clone(), meta("note"));
        n2.security = Some(SecurityDescriptor {
            owner: "alice".into(),
            acl: vec![AclEntry {
                principal: "bob".into(),
                action: Action::Read,
                effect: Effect::Deny,
            }],
            classification: None,
        });
        let n2 = k.remember(n2).unwrap().koid;

        GraphEngine::relate(&k, RelateRequest::new(&alice, n1, n2, "references")).unwrap();

        let hits = GraphEngine::traverse(&k, TraverseQuery::new(&bob, n1).with_depth(1)).unwrap();
        assert!(hits.is_empty(), "bob can read n1 but must not see n2");
    }

    #[test]
    fn graph_engine_api_trait_syntax() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");

        let r = k
            .relate(RelateRequest::new(&a, n1, n2, "references"))
            .unwrap();
        assert_eq!(r.version, 2);

        let hits = k
            .traverse(TraverseQuery::new(&a, n1).with_depth(1))
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].koid, n2);
    }

    #[test]
    fn outbound_edges_uses_index() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");
        let n3 = create(&k, &a, "note");

        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();
        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n3, "cites")).unwrap();

        let edges = k.outbound_edges(&n1, None).unwrap();
        assert_eq!(edges.len(), 2);
        let types: Vec<&str> = edges.iter().map(|(rt, _)| rt.as_str()).collect();
        assert!(types.contains(&"references"));
        assert!(types.contains(&"cites"));

        let filtered = k.outbound_edges(&n1, Some("references")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "references");
        assert_eq!(filtered[0].1, n2);
    }

    #[test]
    fn inbound_edges_uses_index() {
        let k = mk();
        let a = subject("alice");
        let n1 = create(&k, &a, "note");
        let n2 = create(&k, &a, "note");

        GraphEngine::relate(&k, RelateRequest::new(&a, n1, n2, "references")).unwrap();

        let edges = k.inbound_edges(&n2, None).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, "references");
        assert_eq!(edges[0].1, n1);
    }
}
