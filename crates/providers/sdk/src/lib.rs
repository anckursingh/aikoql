//! Mnemosyne Provider SDK — trait-based interface for external data system connectors.
//!
//! Providers connect external systems (PostgreSQL, Neo4j, MongoDB, etc.) to the
//! Knowledge Kernel. They implement the `Provider` trait and expose schema
//! discovery, data ingestion, and CDC capabilities through a common interface.
//!
//! ## Architecture
//!
//! ```text
//! Provider → Provider SDK → Knowledge Adapter → Knowledge Kernel
//! ```
//!
//! A provider MUST NOT directly manipulate storage internals. All data flows
//! through the Knowledge Kernel's `remember` / `forget` / `relate` syscalls.

use mnemosyne_kernel::knowledge::kom::KResult;
use std::collections::HashMap;

/// Schema mapping from external system to Mnemosyne types.
#[derive(Clone, Debug, Default)]
pub struct ProviderSchema {
    /// External table/collection name → Mnemosyne type_name.
    pub type_mappings: HashMap<String, String>,
    /// External column/field name → Mnemosyne property name per type.
    pub property_mappings: HashMap<String, HashMap<String, String>>,
}

/// Result of connecting to and introspecting an external system.
#[derive(Clone, Debug, Default)]
pub struct ProviderMetadata {
    pub name: String,
    pub version: String,
    pub tables: Vec<String>,
    pub estimated_row_count: u64,
}

/// The core provider interface. All external data system connectors
/// implement this trait to integrate with the Knowledge Kernel.
pub trait Provider: Send + Sync {
    /// Human-readable provider name (e.g. "postgres", "neo4j").
    fn name(&self) -> &str;

    /// Connect to the external system and return metadata.
    fn connect(&mut self) -> KResult<ProviderMetadata>;

    /// Discover the external system's schema and return type/property mappings.
    fn discover_schema(&self) -> KResult<ProviderSchema>;

    /// Ingest data from the external system into the Knowledge Kernel.
    /// Each row becomes a Knowledge Object via `kernel.remember()`.
    fn ingest(
        &self,
        kernel: &mnemosyne_kernel::transaction::kernel::Kernel,
        schema: &ProviderSchema,
    ) -> KResult<IngestStats>;

    /// Whether this provider supports Change Data Capture (CDC).
    fn supports_cdc(&self) -> bool {
        false
    }
}

/// Statistics from an ingestion run.
#[derive(Clone, Debug, Default)]
pub struct IngestStats {
    pub nodes_created: u64,
    pub nodes_updated: u64,
    pub relationships_created: u64,
    pub errors: u64,
}
