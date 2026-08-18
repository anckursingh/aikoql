//! MCP tool implementations, one domain per module (R7).
//! Extracted from main.rs. No behavior changes.

pub(crate) mod admin;
pub(crate) mod agent_knowledge;
pub(crate) mod constraints;
pub(crate) mod deployment;
pub(crate) mod evaluation;
pub(crate) mod ingestion;
pub(crate) mod knowledge;
pub(crate) mod memory;
pub(crate) mod query;

pub(crate) use admin::*;
pub(crate) use agent_knowledge::*;
pub(crate) use constraints::*;
pub(crate) use deployment::*;
pub(crate) use evaluation::*;
pub(crate) use ingestion::*;
pub(crate) use knowledge::*;
pub(crate) use memory::*;
pub(crate) use query::*;
