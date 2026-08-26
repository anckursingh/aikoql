//! MVP-QA-002 Suite G — Rust state-machine / property tests
//! (QA2-PROP-001..004).
//!
//! One seeded random driver per invariant class. Each op is executed
//! against the real kernel while a reference model in this file mirrors the
//! expected committed state; after every op the model is compared against
//! the store (get + trace + interval invariants). On failure the panic
//! names the seed and the op index — rerunning with that seed reproduces.
//! ponytail: no shrinker (xorshift seed IS the repro); proptest strategies
//! would add a strategy layer without new coverage here.

use aikoql_kernel::*;
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Deterministic RNG (xorshift64*)
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pct(&mut self) -> u64 {
        self.next() % 100
    }
}

// ---------------------------------------------------------------------------
// Reference model
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MKo {
    version: u64,
    props: PropertyMap,
    rels: Vec<(String, KOID)>,
    deleted: bool,
    superseded: bool,
}

#[derive(Clone, Default)]
struct Model {
    kos: HashMap<KOID, MKo>,
    keys: Vec<KOID>,
}

impl Model {
    fn live(&self) -> Vec<KOID> {
        self.keys
            .iter()
            .copied()
            .filter(|k| !self.kos[k].deleted)
            .collect()
    }
}

fn ctx() -> KnowledgeContext {
    KnowledgeContext::new(Subject::new("alice"))
}

fn meta() -> Metadata {
    Metadata {
        type_name: "prop_ko".into(),
        tenant: None,
        schema_version: 1,
        tags: vec![],
    }
}

fn props(seq: u64) -> PropertyMap {
    let mut p = PropertyMap::new();
    p.insert("seq".into(), Value::Int(seq as i64));
    p
}

fn ev() -> Evidence {
    Evidence::new("qa2-prop", EvidenceMethod::DocExtraction)
}

/// Compare the whole model against the store. THE invariant — every
/// committed head must match the model exactly (properties, version,
/// relationships, lifecycle), every version must be traceable, and no
/// committed interval may run backwards.
fn check(k: &Kernel, m: &Model, seed: u64, op: usize) {
    for (koid, mk) in &m.kos {
        let head = k.get(ctx(), koid).unwrap_or_else(|e| {
            panic!(
                "seed {seed:#x} op {op}: get {} failed: {e:?}",
                koid.to_hex()
            )
        });
        assert_eq!(
            head.version,
            mk.version,
            "seed {seed:#x} op {op}: version mismatch on {}",
            koid.to_hex()
        );
        assert_eq!(
            head.properties,
            mk.props,
            "seed {seed:#x} op {op}: props mismatch on {}",
            koid.to_hex()
        );
        let rels: Vec<(String, KOID)> = head
            .relationships
            .iter()
            .map(|r| (r.rel_type.clone(), r.target))
            .collect();
        assert_eq!(
            rels,
            mk.rels,
            "seed {seed:#x} op {op}: relationships mismatch on {}",
            koid.to_hex()
        );
        assert_eq!(
            head.lifecycle.state == LifecycleState::Deleted,
            mk.deleted,
            "seed {seed:#x} op {op}: lifecycle mismatch on {}",
            koid.to_hex()
        );
        if mk.superseded {
            assert_eq!(
                head.epistemic_status(),
                EpistemicStatus::Superseded,
                "seed {seed:#x} op {op}: superseded KO lost its status"
            );
            assert!(
                head.valid_to().is_some(),
                "seed {seed:#x} op {op}: superseded KO was not closed"
            );
        }
        let tr = k.trace(ctx(), koid).unwrap_or_else(|e| {
            panic!(
                "seed {seed:#x} op {op}: trace {} failed: {e:?}",
                koid.to_hex()
            )
        });
        assert_eq!(
            tr.versions.len(),
            mk.version as usize,
            "seed {seed:#x} op {op}: lineage length {} != version {} on {}",
            tr.versions.len(),
            mk.version,
            koid.to_hex()
        );
        // Temporal invariant: a committed interval must never invert.
        if let (Some(f), Some(t)) = (head.valid_from(), head.valid_to()) {
            assert!(
                f <= t,
                "seed {seed:#x} op {op}: inverted interval [{f},{t}] on {}",
                koid.to_hex()
            );
        }
    }
}

fn create_op(k: &Kernel, m: &mut Model, seq: u64, strict: bool) {
    let mut r = RememberRequest::create(ctx(), meta());
    r.properties = props(seq);
    if strict {
        r.referential_policy = ReferentialPolicy::Strict;
    }
    let rem = k.remember(r).unwrap();
    m.keys.push(rem.koid);
    m.kos.insert(
        rem.koid,
        MKo {
            version: rem.version,
            props: props(seq),
            rels: vec![],
            deleted: false,
            superseded: false,
        },
    );
}

// ---------------------------------------------------------------------------
// QA2-PROP-001 — random KO lifecycle: create/update/delete/restore/
// supersede/reingest/relate/unrelate, all invariants hold
// ---------------------------------------------------------------------------

#[test]
fn w2_prop_001_random_ko_lifecycle_all_invariants_hold() {
    const SEED: u64 = 0xC0FFEE;
    let engine = Arc::new(MemoryEngine::new());
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(engine.clone(), clock.clone(), SEED).unwrap();
    let mut rng = Rng(SEED);
    let mut m = Model::default();
    let mut seq = 0u64;
    let mut idem_orig: Option<(String, Remembered, KOID)> = None;

    for op in 0..400usize {
        // "restore": a snapshot/restore cycle at a fixed cadence — the model
        // rolls back to the checkpoint, then keeps mutating. Uses the real
        // engine snapshot/restore machinery (kernel-level restore).
        if op % 97 == 96 {
            let path = std::env::temp_dir().join("qa2_prop_001_snap.redb");
            let checkpoint = m.clone();
            engine.snapshot_to(&path).unwrap();
            k.restore_store_from(&path).unwrap();
            m = checkpoint;
            check(&k, &m, SEED, op);
            continue;
        }

        let roll = rng.pct();
        let live = m.live();
        let can_mutate = live.len() >= 2;
        if roll < 30 || m.kos.is_empty() || (!can_mutate && roll < 70) {
            seq += 1;
            create_op(&k, &mut m, seq, false);
        } else if roll < 50 && can_mutate {
            let koid = live[rng.below(live.len())];
            if idem_orig
                .as_ref()
                .map(|(_, _, k)| *k == koid)
                .unwrap_or(false)
            {
                continue;
            }
            seq += 1;
            let mut r = RememberRequest::update(ctx(), koid, meta());
            r.properties = props(seq);
            // remember() replaces relationships wholesale — an update must
            // restate the edges it keeps (API contract).
            r.relationships = m.kos[&koid]
                .rels
                .iter()
                .map(|(t, k)| RelationshipRef {
                    rel_type: t.clone(),
                    target: *k,
                    direction: Direction::Outbound,
                })
                .collect();
            let rem = k.remember(r).unwrap();
            let mk = m.kos.get_mut(&koid).unwrap();
            assert_eq!(
                rem.version,
                mk.version + 1,
                "seed {SEED:#x} op {op}: update must bump version by exactly 1"
            );
            mk.version = rem.version;
            mk.props = props(seq);
        } else if roll < 58 && can_mutate {
            let koid = live[rng.below(live.len())];
            if idem_orig
                .as_ref()
                .map(|(_, _, k)| *k == koid)
                .unwrap_or(false)
            {
                continue;
            }
            let f = k
                .forget(ctx(), &koid, ForgetMode::Tombstone, None, None)
                .unwrap();
            let mk = m.kos.get_mut(&koid).unwrap();
            assert_eq!(f.version, mk.version + 1);
            mk.version = f.version;
            mk.deleted = true;
        } else if roll < 66 && can_mutate {
            let candidates: Vec<KOID> = live
                .iter()
                .copied()
                .filter(|k| !m.kos[k].superseded)
                .collect();
            let Some(&koid) = candidates.get(rng.below(candidates.len().max(1))) else {
                check(&k, &m, SEED, op);
                continue;
            };
            if idem_orig
                .as_ref()
                .map(|(_, _, k)| *k == koid)
                .unwrap_or(false)
            {
                continue;
            }
            let mut req = SupersedeRequest::new(ctx(), koid, "prop_ko");
            req.properties = props(seq);
            req.evidence = vec![ev()];
            let res = k.supersede(req).unwrap();
            let old = m.kos.get_mut(&koid).unwrap();
            old.version += 1;
            old.superseded = true;
            old.rels.push(("supersedes".into(), res.new));
            m.keys.push(res.new);
            m.kos.insert(
                res.new,
                MKo {
                    version: 1,
                    props: props(seq),
                    rels: vec![],
                    deleted: false,
                    superseded: false,
                },
            );
            seq += 1;
        } else if roll < 76 {
            // reingest: same idempotency key replays the original commit.
            match &idem_orig {
                None => {
                    let mut r = RememberRequest::create(ctx(), meta());
                    r.idempotency_key = Some("reingest-k".into());
                    r.properties = props(seq);
                    let rem = k.remember(r).unwrap();
                    idem_orig = Some(("reingest-k".into(), rem, rem.koid));
                    m.keys.push(rem.koid);
                    m.kos.insert(
                        rem.koid,
                        MKo {
                            version: rem.version,
                            props: props(seq),
                            rels: vec![],
                            deleted: false,
                            superseded: false,
                        },
                    );
                }
                Some((key, orig, koid)) => {
                    let mut r = RememberRequest::create(ctx(), meta());
                    r.idempotency_key = Some(key.clone());
                    r.properties = props(seq);
                    let rem = k.remember(r).unwrap();
                    assert_eq!(
                        rem, *orig,
                        "seed {SEED:#x} op {op}: reingest must replay, not churn"
                    );
                    assert_eq!(rem.koid, *koid);
                    let mk = m.kos.get_mut(koid).unwrap();
                    assert_eq!(mk.version, orig.version);
                }
            }
        } else if roll < 86 && can_mutate {
            let src = live[rng.below(live.len())];
            let tgt = m.keys[rng.below(m.keys.len())];
            if idem_orig
                .as_ref()
                .map(|(_, _, k)| *k == src)
                .unwrap_or(false)
            {
                continue;
            }
            let mut r = RememberRequest::update(ctx(), src, meta());
            r.properties = m.kos[&src].props.clone();
            let mut rels = m.kos[&src].rels.clone();
            if !rels.iter().any(|(t, k)| *t == "related_to" && *k == tgt) {
                rels.push(("related_to".into(), tgt));
            }
            r.relationships = rels
                .iter()
                .map(|(t, k)| RelationshipRef {
                    rel_type: t.clone(),
                    target: *k,
                    direction: Direction::Outbound,
                })
                .collect();
            let rem = k.remember(r).unwrap();
            let mk = m.kos.get_mut(&src).unwrap();
            mk.version = rem.version;
            mk.rels = rels;
        } else if roll < 92 {
            // Kernel-managed edges (supersedes/derived_from/contradicts) are
            // carried forward by remember() on update and cannot be removed
            // by a caller — only caller-owned edges are unrelate-eligible.
            let caller_owned =
                |t: &str| t != "supersedes" && t != "derived_from" && t != "contradicts";
            let candidates: Vec<KOID> = live
                .iter()
                .copied()
                .filter(|k| {
                    m.kos[k]
                        .rels
                        .last()
                        .map(|(t, _)| caller_owned(t))
                        .unwrap_or(false)
                })
                .collect();
            if let Some(&src) = candidates.get(rng.below(candidates.len().max(1))) {
                if idem_orig
                    .as_ref()
                    .map(|(_, _, k)| *k == src)
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut r = RememberRequest::update(ctx(), src, meta());
                r.properties = m.kos[&src].props.clone();
                let mut rels = m.kos[&src].rels.clone();
                let pos = rels
                    .iter()
                    .rposition(|(t, _)| caller_owned(t))
                    .expect("candidate filter guarantees a caller-owned edge");
                rels.remove(pos);
                r.relationships = rels
                    .iter()
                    .map(|(t, k)| RelationshipRef {
                        rel_type: t.clone(),
                        target: *k,
                        direction: Direction::Outbound,
                    })
                    .collect();
                let rem = k.remember(r).unwrap();
                let mk = m.kos.get_mut(&src).unwrap();
                mk.version = rem.version;
                mk.rels = rels;
            }
        }
        // else: query-only slot — the invariants below are the query pin.
        check(&k, &m, SEED, op);
    }
}

// ---------------------------------------------------------------------------
// QA2-PROP-002 — random relationship ops: no orphan endpoints, no
// impossible relationship state
// ---------------------------------------------------------------------------

#[test]
fn w2_prop_002_random_relationship_ops_no_orphan_endpoints() {
    const SEED: u64 = 0x05EC_0000;
    let (k, _c) = {
        let engine = Arc::new(MemoryEngine::new());
        (
            Kernel::open(engine, Arc::new(ManualClock::new(10_000)), SEED).unwrap(),
            (),
        )
    };
    let mut rng = Rng(SEED);
    let mut m = Model::default();
    let mut seq = 0u64;
    for _ in 0..6 {
        seq += 1;
        create_op(&k, &mut m, seq, true);
    }

    for op in 0..300usize {
        let roll = rng.pct();
        let live = m.live();
        if roll < 35 {
            let src = live[rng.below(live.len().max(1))];
            let tgt = m.keys[rng.below(m.keys.len())];
            let mut r = RememberRequest::update(ctx(), src, meta());
            r.referential_policy = ReferentialPolicy::Strict;
            r.properties = m.kos[&src].props.clone();
            let mut rels = m.kos[&src].rels.clone();
            if !rels.iter().any(|(t, k)| *t == "related_to" && *k == tgt) {
                rels.push(("related_to".into(), tgt));
            }
            r.relationships = rels
                .iter()
                .map(|(t, k)| RelationshipRef {
                    rel_type: t.clone(),
                    target: *k,
                    direction: Direction::Outbound,
                })
                .collect();
            let rem = k.remember(r).unwrap();
            let mk = m.kos.get_mut(&src).unwrap();
            mk.version = rem.version;
            mk.rels = rels;
        } else if roll < 60 {
            let candidates: Vec<KOID> = live
                .iter()
                .copied()
                .filter(|k| !m.kos[k].rels.is_empty())
                .collect();
            if let Some(&src) = candidates.get(rng.below(candidates.len().max(1))) {
                let mut r = RememberRequest::update(ctx(), src, meta());
                r.referential_policy = ReferentialPolicy::Strict;
                r.properties = m.kos[&src].props.clone();
                let mut rels = m.kos[&src].rels.clone();
                rels.pop();
                r.relationships = rels
                    .iter()
                    .map(|(t, k)| RelationshipRef {
                        rel_type: t.clone(),
                        target: *k,
                        direction: Direction::Outbound,
                    })
                    .collect();
                let rem = k.remember(r).unwrap();
                let mk = m.kos.get_mut(&src).unwrap();
                mk.version = rem.version;
                mk.rels = rels;
            }
        } else if roll < 70 && live.len() >= 3 {
            let koid = live[rng.below(live.len())];
            let f = k
                .forget(ctx(), &koid, ForgetMode::Tombstone, None, None)
                .unwrap();
            let mk = m.kos.get_mut(&koid).unwrap();
            mk.version = f.version;
            mk.deleted = true;
        } else if roll < 80 {
            seq += 1;
            create_op(&k, &mut m, seq, true);
        }
        // Invariants: model/head agreement (check), plus the orphan pin —
        // every relationship endpoint must still resolve to a head object
        // (a tombstone keeps its head, so delete never orphans a target).
        for (koid, mk) in &m.kos {
            let mut edges: Vec<(String, KOID)> = k.outbound_edges(koid, None).unwrap();
            edges.sort();
            let mut model_edges = mk.rels.clone();
            model_edges.sort();
            assert_eq!(
                edges,
                model_edges,
                "seed {SEED:#x} op {op}: edge index drifted from head on {}",
                koid.to_hex()
            );
            for (_, tgt) in &mk.rels {
                assert!(
                    k.get(ctx(), tgt).is_ok(),
                    "seed {SEED:#x} op {op}: orphan endpoint {} on {}",
                    tgt.to_hex(),
                    koid.to_hex()
                );
            }
        }
        check(&k, &m, SEED, op);
    }
}

// ---------------------------------------------------------------------------
// QA2-PROP-003 — random temporal ops: overlapping validity intervals keep
// temporal invariants
// ---------------------------------------------------------------------------

#[test]
fn w2_prop_003_random_temporal_ops_keep_temporal_invariants() {
    const SEED: u64 = 0x0BEEF;
    let clock = Arc::new(ManualClock::new(10_000));
    let k = Kernel::open(Arc::new(MemoryEngine::new()), clock.clone(), SEED).unwrap();
    let mut rng = Rng(SEED);
    let mut m = Model::default();
    let mut seq = 0u64;

    for op in 0..300usize {
        clock.tick(rng.below(50) as u64 + 1);
        let now = clock.millis();
        let roll = rng.pct();
        let live = m.live();
        if roll < 40 || m.kos.is_empty() {
            seq += 1;
            let mut req = AssertionRequest::new(ctx(), "temporal_claim");
            req.properties = props(seq);
            // Half the claims are future-dated — the supersede pin depends
            // on intervals surviving whatever comes next.
            let offset = rng.below(800) as i64 - 300;
            req.valid_from = Some((now as i64 + offset).max(0) as u64);
            req.authority = Some("documentation".into());
            req.evidence = vec![ev()];
            let rem = k.assert_knowledge(req).unwrap();
            m.keys.push(rem.koid);
            m.kos.insert(
                rem.koid,
                MKo {
                    version: 1,
                    props: props(seq),
                    rels: vec![],
                    deleted: false,
                    superseded: false,
                },
            );
        } else if roll < 60 && live.len() >= 2 {
            let candidates: Vec<KOID> = live
                .iter()
                .copied()
                .filter(|k| !m.kos[k].superseded)
                .collect();
            let Some(&koid) = candidates.get(rng.below(candidates.len().max(1))) else {
                check(&k, &m, SEED, op);
                continue;
            };
            let mut req = SupersedeRequest::new(ctx(), koid, "temporal_claim");
            let sprops = props(seq);
            req.properties = sprops.clone();
            req.evidence = vec![ev()];
            seq += 1;
            // clean rejection is a valid outcome; state must be unchanged
            if let Ok(res) = k.supersede(req) {
                let old = m.kos.get_mut(&koid).unwrap();
                old.version += 1;
                old.superseded = true;
                old.rels.push(("supersedes".into(), res.new));
                m.keys.push(res.new);
                m.kos.insert(
                    res.new,
                    MKo {
                        version: 1,
                        props: sprops,
                        rels: vec![],
                        deleted: false,
                        superseded: false,
                    },
                );
            }
        } else if roll < 80 {
            seq += 1;
            let mut r = RememberRequest::create(ctx(), meta());
            r.properties = props(seq);
            let horizon = rng.below(2000) as u64 + 1;
            let rem = k.remember_retained(r, horizon).unwrap();
            m.keys.push(rem.koid);
            m.kos.insert(
                rem.koid,
                MKo {
                    version: 1,
                    props: props(seq),
                    rels: vec![],
                    deleted: false,
                    superseded: false,
                },
            );
        } else if live.len() >= 2 {
            let koid = live[rng.below(live.len())];
            seq += 1;
            let mut r = RememberRequest::update(ctx(), koid, meta());
            r.properties = props(seq);
            // Same restate contract as PROP-001: an update replaces the
            // relationship list wholesale.
            r.relationships = m.kos[&koid]
                .rels
                .iter()
                .map(|(t, k)| RelationshipRef {
                    rel_type: t.clone(),
                    target: *k,
                    direction: Direction::Outbound,
                })
                .collect();
            let rem = k.remember(r).unwrap();
            let mk = m.kos.get_mut(&koid).unwrap();
            mk.version = rem.version;
            mk.props = props(seq);
        }
        check(&k, &m, SEED, op);
    }
}

// ---------------------------------------------------------------------------
// QA2-PROP-004 — random query/mutation interleaving: every result matches
// the transactionally visible committed state
// ---------------------------------------------------------------------------

#[test]
fn w2_prop_004_random_query_mutation_interleaving_consistent() {
    const SEED: u64 = 0xDEAD;
    let (k, _c) = {
        let engine = Arc::new(MemoryEngine::new());
        (
            Kernel::open(engine, Arc::new(ManualClock::new(10_000)), SEED).unwrap(),
            (),
        )
    };
    let mut rng = Rng(SEED);
    let mut m = Model::default();
    let mut seq = 0u64;
    seq += 1;
    create_op(&k, &mut m, seq, false);
    seq += 1;
    create_op(&k, &mut m, seq, false);
    let (a, b) = (m.keys[0], m.keys[1]);

    for op in 0..200usize {
        if rng.pct() < 50 {
            // Poisoned transaction: op A is valid, op B carries a wrong
            // expected version — the batch must fail as a whole and leave
            // NO partial state behind.
            seq += 1;
            let mut ra = RememberRequest::update(ctx(), a, meta());
            ra.properties = props(seq);
            let mut rb = RememberRequest::update(ctx(), b, meta());
            rb.properties = props(seq);
            rb.expected_version = Some(m.kos[&b].version + 77);
            let res = k.transact(vec![
                TransactionOp::new(ctx(), ra),
                TransactionOp::new(ctx(), rb),
            ]);
            assert!(
                res.is_err(),
                "seed {SEED:#x} op {op}: poisoned transaction must fail"
            );
            // Query pin: both objects still show the pre-transaction state.
            assert_eq!(k.get(ctx(), &a).unwrap().properties, m.kos[&a].props);
            assert_eq!(k.get(ctx(), &b).unwrap().properties, m.kos[&b].props);
        } else {
            // Clean transaction: both updates commit together.
            seq += 1;
            let mut ra = RememberRequest::update(ctx(), a, meta());
            ra.properties = props(seq);
            let mut rb = RememberRequest::update(ctx(), b, meta());
            rb.properties = props(seq);
            let rems = k
                .transact(vec![
                    TransactionOp::new(ctx(), ra),
                    TransactionOp::new(ctx(), rb),
                ])
                .unwrap();
            assert_eq!(rems.len(), 2, "seed {SEED:#x} op {op}: two results");
            for (koid, rem) in [(a, &rems[0]), (b, &rems[1])] {
                let mk = m.kos.get_mut(&koid).unwrap();
                assert_eq!(rem.version, mk.version + 1);
                mk.version = rem.version;
                mk.props = props(seq);
            }
        }
        // Interleaved query: read both objects and match the model.
        for koid in [a, b] {
            let head = k.get(ctx(), &koid).unwrap();
            let mk = &m.kos[&koid];
            assert_eq!(head.version, mk.version, "seed {SEED:#x} op {op}");
            assert_eq!(head.properties, mk.props, "seed {SEED:#x} op {op}");
        }
        check(&k, &m, SEED, op);
    }
}
