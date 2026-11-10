//! Deterministic object ids shared by the importers, resolution and document commit: the same
//! input always names the same object (so a repeat run updates instead of duplicating), and an
//! id that was tombstoned moves to a fresh generation (the kernel never revives a tombstone).

use crate::{Kernel, Subject, KOID};

/// 128 bits from two independent FNV-1a passes over (source, container, key values).
pub(crate) fn identity_koid(source: &str, container: &str, parts: &[String]) -> KOID {
    let text = format!("{source}\u{1f}{container}\u{1f}{}", parts.join("\u{1e}"));
    let fnv = |seed: u64| {
        text.bytes().fold(seed, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let mut koid = [0u8; 16];
    koid[..8].copy_from_slice(&fnv(0xcbf2_9ce4_8422_2325).to_be_bytes());
    koid[8..].copy_from_slice(&fnv(0x8422_2325_cbf2_9ce4).to_be_bytes());
    KOID(koid)
}

/// The first id of `id_of(0), id_of(1), ...` that is not a tombstone. The kernel never
/// revives a tombstoned id (a write to it stays deleted), so a row that was retired and
/// then comes back takes the next generation of its id: still deterministic for a given
/// history, and what an earlier version said stays under the old id.
pub(crate) fn live_koid(k: &Kernel, id_of: impl Fn(u32) -> KOID) -> KOID {
    let reader = crate::KnowledgeContext::from(Subject::with_roles("system", &["admin"]));
    let dead = |id: &KOID| {
        k.get(reader.clone(), id)
            .is_ok_and(|h| h.lifecycle.state == crate::LifecycleState::Deleted)
    };
    // 1000 generations is far past any real history: a loop bound, not a limit anyone meets
    (0..1000)
        .map(&id_of)
        .find(|id| !dead(id))
        .unwrap_or_else(|| id_of(1000))
}
