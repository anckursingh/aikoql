//! Knowledge Lifecycle submodule (MRFC-0001 §6).
//!
//! Hosts the `LifecycleManager` (state machine validation), `SchemaRegistry`
//! (type validation), `ConstraintEvaluator` (domain + check constraints),
//! and re-exports core lifecycle types.

pub mod constraint;
pub mod manager;
pub mod schema;

pub use crate::knowledge::kom::{Lifecycle, LifecycleState, Origin};
pub use constraint::ConstraintEvaluator;
pub use manager::LifecycleManager;
pub use schema::SchemaRegistry;
