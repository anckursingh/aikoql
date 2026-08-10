//! Knowledge Storage submodule.
//!
//! Owns the `StorageEngine` trait, durable backends, the key-hiding repository,
//! and the optional in-memory cache.

pub mod cache;
pub mod encrypted;
pub mod repository;
pub mod store;
pub mod store_redb;

pub use cache::KnowledgeCache;
pub use encrypted::EncryptedStore;
pub use repository::KnowledgeRepository;
