//! Knowledge Security submodule (MRFC-0001 §12).
//!
//! Increment-1 keeps ACL/RBAC types in `knowledge::kom`. This module re-exports
//! those types and hosts the `AuthManager` that evaluates authorization.

pub mod audit;
pub mod auth;
pub mod crypto;
pub mod envelope;
pub mod field_crypto;
pub mod kms;
pub mod signing;
pub mod tenant;

pub use crate::knowledge::kom::{AclEntry, Action, Effect, SecurityDescriptor};
pub use audit::{KeyAuditLog, KeyEvent, KeyEventKind};
pub use auth::AuthManager;
pub use crypto::{Aes256Gcm, ChaCha20Poly1305, Crypto, CryptoProvider};
pub use envelope::Envelope;
pub use field_crypto::{ComplianceSummary, EncryptionPolicy, FieldCrypto};
pub use kms::{KeyManager, LocalKms};
pub use signing::{Signer, SigningKey};
pub use tenant::{TenantManager, TenantQuota};
