//! Knowledge Lifecycle submodule (MRFC-0001 §6).
//!
//! Hosts the `LifecycleManager` (state machine validation), `SchemaRegistry`
//! (type validation), and re-exports core lifecycle types.

pub mod manager;
pub mod schema;

pub use crate::knowledge::kom::{Lifecycle, LifecycleState, Origin};
pub use manager::LifecycleManager;
pub use schema::SchemaRegistry;
