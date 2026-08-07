//! Lifecycle Manager — owns the KO lifecycle state machine
//! (MRFC-0005 §Knowledge Kernel).
//!
//! Centralizes lifecycle transition validation (draft→active→verified→
//! archived→deleted) and the `evolve` syscall logic.

use crate::knowledge::kom::*;

pub struct LifecycleManager;

impl LifecycleManager {
    /// Validate a state transition (MRFC-0001 §6).
    /// Legal: Draft→Active→Verified→Archived→Deleted, Draft→Deleted.
    pub fn validate_transition(from: LifecycleState, to: LifecycleState) -> KResult<()> {
        if from.can_transition(to) {
            Ok(())
        } else {
            Err(KError::InvalidState { from, to })
        }
    }

    /// Apply a lifecycle transition to a KO, returning the new state.
    /// Does NOT persist — the caller must commit via the kernel.
    pub fn transition(ko: &mut KnowledgeObject, to: LifecycleState) -> KResult<LifecycleState> {
        Self::validate_transition(ko.lifecycle.state, to)?;
        ko.lifecycle.state = to;
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_draft_to_active() {
        assert!(LifecycleManager::validate_transition(
            LifecycleState::Draft,
            LifecycleState::Active
        )
        .is_ok());
    }

    #[test]
    fn illegal_verified_to_draft() {
        assert!(LifecycleManager::validate_transition(
            LifecycleState::Verified,
            LifecycleState::Draft
        )
        .is_err());
    }
}
