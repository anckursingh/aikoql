//! P4-M2 — physical handles (TDD-ID-001/002): the opaque stable physical
//! reference between the placement directory and segment/block coordinates.
//! Upper layers hold `PhysicalHandle`s, never `SegmentId`/`BlockId`; a
//! relocation flips the handle while `ReplicaId` never changes; a stale
//! handle fails closed at use time.
//!
//! Safety invariant: `location()` never trusts the registry's own table —
//! it re-reads the live source and demands the same generation, so a
//! superseded handle can never return an older value. A handle the registry
//! never issued (or has retired) fails closed too.

use crate::format::FormatError;
use crate::identity::ReplicaId;
use crate::placement::directory::{PhysicalLocation, Placement};
use std::collections::HashMap;

/// TDD-ID-001 — the opaque stable physical reference. The value is a
/// monotonic registry counter; its encoding is private to the registry.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalHandle(pub u64);

/// TDD-ID-002 — the resolver contract. `resolve_many` is mandatory, not
/// optional (the review's requirement).
pub trait PhysicalResolver: Send + Sync {
    fn resolve(&self, rid: ReplicaId) -> Result<Option<PhysicalHandle>, FormatError>;
    fn resolve_many(&self, rids: &[ReplicaId]) -> Result<Vec<Option<PhysicalHandle>>, FormatError>;
}

/// The live placement view a registry resolves through — the Db's directory
/// (see `LocalPlacementResolver`) or a synthetic map in tests.
pub trait PlacementSource: Send + Sync {
    fn placement(&self, rid: ReplicaId) -> Option<Placement>;
}

/// Shared/borrowed sources are sources too (tests pass `&SyntheticSource`,
/// the Db form is a struct owning an `&Db`).
impl<T: PlacementSource + ?Sized> PlacementSource for &T {
    fn placement(&self, rid: ReplicaId) -> Option<Placement> {
        (**self).placement(rid)
    }
}

/// The one handle implementation. Allocation is interior-mutable by design:
/// the shared form is `Mutex<HandleRegistry<S>>` (the `PhysicalResolver`
/// impl below), while single-threaded tests use the bare struct.
pub struct HandleRegistry<S: PlacementSource> {
    source: S,
    by_handle: HashMap<PhysicalHandle, (ReplicaId, u64 /* placement generation */)>,
    by_rid: HashMap<ReplicaId, PhysicalHandle>,
    next: u64,
}

impl<S: PlacementSource> HandleRegistry<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            by_handle: HashMap::new(),
            by_rid: HashMap::new(),
            next: 1,
        }
    }

    /// Drop every trace of a replica's handle — anyone still holding it
    /// fails closed on `location`.
    fn retire(&mut self, rid: ReplicaId) {
        if let Some(h) = self.by_rid.remove(&rid) {
            self.by_handle.remove(&h);
        }
    }

    /// Resolve a replica to its current handle. `None` = memtable, retired,
    /// or absent (no physical location). A placement-generation change
    /// retires the old handle and allocates a fresh one.
    pub fn resolve(&mut self, rid: ReplicaId) -> Result<Option<PhysicalHandle>, FormatError> {
        match self.source.placement(rid) {
            None => {
                self.retire(rid);
                Ok(None)
            }
            Some(Placement::Memtable { .. } | Placement::Retired { .. }) => {
                self.retire(rid);
                Ok(None)
            }
            Some(Placement::Segment(loc)) => {
                if let Some(h) = self.by_rid.get(&rid) {
                    if let Some((_, gen)) = self.by_handle.get(h) {
                        if *gen == loc.generation {
                            return Ok(Some(*h)); // generation stable — handle stable
                        }
                    }
                }
                self.retire(rid);
                let h = PhysicalHandle(self.next);
                self.next += 1;
                if self.by_handle.contains_key(&h) {
                    // Generation collision — fail closed. Unreachable with a
                    // monotonic counter, but the check is the contract.
                    return Err(FormatError::Corrupt(format!(
                        "physical handle collision at {}",
                        h.0
                    )));
                }
                self.by_handle.insert(h, (rid, loc.generation));
                self.by_rid.insert(rid, h);
                Ok(Some(h))
            }
        }
    }

    /// Elementwise `resolve` — the mandatory batch API.
    pub fn resolve_many(
        &mut self,
        rids: &[ReplicaId],
    ) -> Result<Vec<Option<PhysicalHandle>>, FormatError> {
        rids.iter().map(|r| self.resolve(*r)).collect()
    }

    /// The use-side: handle → current physical location. Re-validates
    /// against the live source on every call — a stale generation or an
    /// unknown handle is an error, never data.
    pub fn location(&self, handle: PhysicalHandle) -> Result<PhysicalLocation, FormatError> {
        let Some((rid, gen)) = self.by_handle.get(&handle) else {
            return Err(FormatError::Stale(format!(
                "physical handle {} unknown or retired",
                handle.0
            )));
        };
        match self.source.placement(*rid) {
            Some(Placement::Segment(loc)) if loc.generation == *gen => Ok(loc),
            _ => Err(FormatError::Stale(format!(
                "physical handle {} stale — placement generation moved",
                handle.0
            ))),
        }
    }
}

/// The shared form: the Db (or any concurrent caller) holds the registry
/// behind a mutex; `location` re-validation means readers can never observe
/// a mix of pre- and post-relocation locations.
impl<S: PlacementSource> PhysicalResolver for std::sync::Mutex<HandleRegistry<S>> {
    fn resolve(&self, rid: ReplicaId) -> Result<Option<PhysicalHandle>, FormatError> {
        self.lock()
            .map_err(|_| FormatError::Locked("handle registry poisoned".into()))?
            .resolve(rid)
    }

    fn resolve_many(&self, rids: &[ReplicaId]) -> Result<Vec<Option<PhysicalHandle>>, FormatError> {
        self.lock()
            .map_err(|_| FormatError::Locked("handle registry poisoned".into()))?
            .resolve_many(rids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ReplicaId;
    use crate::placement::directory::{PhysicalLocation, Placement};
    use crate::placement::{BlockId, SegmentId};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Synthetic placement source: a Mutex map swapped wholesale to model
    /// an atomic relocation batch (one assignment = reader sees old-or-new,
    /// never a mix).
    struct SyntheticSource {
        map: Mutex<HashMap<ReplicaId, Placement>>,
    }

    impl SyntheticSource {
        fn new() -> Self {
            Self {
                map: Mutex::new(HashMap::new()),
            }
        }
        fn set(&self, rid: u64, placement: Placement) {
            self.map.lock().unwrap().insert(ReplicaId(rid), placement);
        }
        fn relocate(&self, batch: &[(u64, Placement)]) {
            let mut map = self.map.lock().unwrap();
            for (rid, p) in batch {
                map.insert(ReplicaId(*rid), *p);
            }
        }
    }

    impl PlacementSource for SyntheticSource {
        fn placement(&self, rid: ReplicaId) -> Option<Placement> {
            self.map.lock().unwrap().get(&rid).copied()
        }
    }

    fn seg_loc(segment: u64, block: u32, entry: u32, generation: u64) -> PhysicalLocation {
        PhysicalLocation {
            segment_id: SegmentId(segment),
            block_id: BlockId(block),
            entry_offset: entry,
            generation,
        }
    }

    #[test]
    fn phy001_stale_generation_fail_closed() {
        let src = SyntheticSource::new();
        src.set(1, Placement::Segment(seg_loc(10, 0, 0, 5)));
        let mut reg = HandleRegistry::new(&src);

        let h1 = reg.resolve(ReplicaId(1)).unwrap().expect("handle");
        // Relocation: same rid, new generation — the handle must change.
        src.set(1, Placement::Segment(seg_loc(11, 1, 0, 6)));
        let h2 = reg.resolve(ReplicaId(1)).unwrap().expect("new handle");
        assert_ne!(h1, h2, "relocation must flip the handle");

        // The stale handle fails closed — never the old location.
        assert!(
            matches!(reg.location(h1), Err(FormatError::Stale(_))),
            "stale handle must be rejected"
        );
        assert_eq!(reg.location(h2).unwrap(), seg_loc(11, 1, 0, 6));
    }

    #[test]
    fn phy002_resolve_many_equals_scalar() {
        let src = SyntheticSource::new();
        src.set(1, Placement::Segment(seg_loc(10, 0, 0, 1)));
        src.set(2, Placement::Memtable { generation: 1 });
        src.set(3, Placement::Retired { generation: 1 });
        // rid 4 absent
        let mut reg = HandleRegistry::new(&src);
        let rids: Vec<ReplicaId> = (1..=4).map(ReplicaId).collect();

        let batch = reg.resolve_many(&rids).unwrap();
        let scalar: Vec<Option<PhysicalHandle>> =
            rids.iter().map(|r| reg.resolve(*r).unwrap()).collect();
        assert_eq!(batch, scalar);
        assert!(batch[0].is_some());
        assert!(batch[1].is_none()); // memtable has no physical handle
        assert!(batch[2].is_none()); // retired has no physical handle
        assert!(batch[3].is_none()); // absent
    }

    #[test]
    fn phy004_resolve_many_equals_scalar_at_scale() {
        // 1M oracle zero divergence (env PHY_NIGHTLY=1m); 100K by default.
        // The oracle is the elementwise scalar comparison — batch and
        // scalar share one code path, and this pins that the API answers
        // identically at scale (the M25 lesson: batch must be exercised).
        let n: usize = if std::env::var("PHY_NIGHTLY").as_deref() == Ok("1m") {
            1_000_000
        } else {
            100_000
        };
        let src = SyntheticSource::new();
        for rid in 0..n as u64 {
            src.set(
                rid,
                Placement::Segment(seg_loc(1000 + rid / 1024, 0, (rid % 1024) as u32, 1)),
            );
        }
        let mut reg = HandleRegistry::new(&src);
        let rids: Vec<ReplicaId> = (0..n as u64).map(ReplicaId).collect();

        let batch = reg.resolve_many(&rids).unwrap();
        for (i, r) in rids.iter().enumerate() {
            let scalar = reg.resolve(*r).unwrap();
            assert_eq!(scalar, batch[i], "divergence at index {i}");
        }
        assert!(batch.iter().all(Option::is_some));
        // Every handle still resolves to its own location (no handle reuse).
        for (i, h) in batch.iter().enumerate() {
            let loc = reg.location(h.expect("handle")).unwrap();
            assert_eq!(
                loc,
                seg_loc(1000 + (i as u64) / 1024, 0, (i as u64 % 1024) as u32, 1)
            );
        }
    }

    #[test]
    fn phy003_relocation_never_mixes_old_and_new() {
        let src = SyntheticSource::new();
        for rid in 1..=10u64 {
            src.set(rid, Placement::Segment(seg_loc(100, 0, rid as u32, 1)));
        }
        let mut reg = HandleRegistry::new(&src);
        let rids: Vec<ReplicaId> = (1..=10).map(ReplicaId).collect();
        let old_handles: Vec<PhysicalHandle> = rids
            .iter()
            .map(|r| reg.resolve(*r).unwrap().expect("handle"))
            .collect();

        // Atomic relocation batch: one swap, all rids, new generation.
        let batch: Vec<(u64, Placement)> = (1..=10u64)
            .map(|rid| (rid, Placement::Segment(seg_loc(200, 1, rid as u32, 2))))
            .collect();
        src.relocate(&batch);

        // Every pre-batch handle is stale — none returns any location.
        for h in &old_handles {
            assert!(
                matches!(reg.location(*h), Err(FormatError::Stale(_))),
                "pre-relocation handle must not resolve post-relocation"
            );
        }
        // Every post-batch handle resolves to exactly the new location.
        for (i, rid) in (1..=10u64).enumerate() {
            let h = reg.resolve(ReplicaId(rid)).unwrap().expect("new handle");
            assert_ne!(h, old_handles[i], "new generation, new handle");
            assert_eq!(
                reg.location(h).unwrap(),
                seg_loc(200, 1, rid as u32, 2),
                "new handle must carry the new location"
            );
        }
    }
}
