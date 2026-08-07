//! Schema Registry — in-memory type schemas for validation.
//!
//! Increment-1 stores schemas in memory only (MRFC-0001). The kernel calls
//! `SchemaRegistry::validate` before committing a new version.

use crate::knowledge::kom::{KResult, KnowledgeObject, Schema};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct SchemaRegistry {
    schemas: HashMap<String, Schema>,
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, schema: Schema) {
        self.schemas.insert(schema.type_name.clone(), schema);
    }

    pub fn get(&self, type_name: &str) -> Option<&Schema> {
        self.schemas.get(type_name)
    }

    /// Validate an object against its registered schema, if one exists.
    /// Always runs the built-in `KnowledgeObject::validate` checks first.
    pub fn validate(&self, ko: &KnowledgeObject) -> KResult<()> {
        ko.validate()?;
        if let Some(schema) = self.schemas.get(&ko.metadata.type_name) {
            ko.validate_against(schema)?;
        }
        Ok(())
    }
}
