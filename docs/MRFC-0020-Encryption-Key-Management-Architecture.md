# MRFC-0020: Encryption & Key Management Architecture

**RFC ID:** MRFC-0020
**Version:** 1.0
**Status:** Draft

## Purpose
Defines the layered encryption architecture for aikoql. Encryption is treated as a platform subsystem rather than a storage feature.

## Goals
- Zero Trust
- Defense in Depth
- Crypto Agility
- Envelope Encryption
- Tenant Isolation
- Online Key Rotation
- Field-level Policies
- Auditability
- Compliance (GDPR, HIPAA, PCI DSS)

## Threat Model
Protect against:
- Stolen disks
- Snapshot leakage
- Insider access
- Backup theft
- Cross-tenant attacks
- Tampered WAL
- Key compromise

Out of scope (V1):
- Homomorphic encryption
- Encrypted ANN/vector search
- Quantum-safe production deployment

## Layered Model

Application
→ Optional Application Encryption
→ Knowledge Encryption (field/object)
→ Storage Encryption (page/WAL/checkpoint)
→ Disk Encryption

Each layer is independent.

## Encryption Framework

crates/security/
- crypto/
- kms/
- envelope/
- policy/
- rotation/
- audit/
- providers/

Shared by Knowledge Kernel and Storage Kernel.

## Key Hierarchy

Root(KMS/HSM)
→ Master
→ Tenant
→ Database
→ Object
→ Field

Envelope encryption is mandatory.

## Storage Encryption

Encrypt:
- Pages
- WAL
- Checkpoints
- Snapshots
- Backups
- Metadata

Write:
Compress → Encrypt → AEAD → Write

Read:
Read → Verify → Decrypt → Decompress

## Knowledge Encryption

Policy driven:
- Object encryption
- Field encryption
- Relationship metadata
- Provenance encryption

Example:
salary=encrypted
ssn=encrypted
city=plaintext

## Crypto Provider

trait CryptoProvider:
- encrypt
- decrypt
- generate_key
- rotate

Algorithms:
- AES-256-GCM
- ChaCha20-Poly1305

Future:
- PQC
- Confidential Computing

## Key Providers

- Local
- AWS KMS
- Azure Key Vault
- GCP KMS
- HashiCorp Vault
- HSM

## Page Format

Header
Nonce
Ciphertext
Auth Tag
Checksum

## Rotation

Manual
Scheduled
Rolling
Online
Per Tenant

No downtime.

## Audit

Emit immutable events:
- Key creation
- Rotation
- Encryption
- Decryption
- Policy updates
- Failures

## HLD Integration

Applications
→ API
→ Compiler
→ Runtime
→ Knowledge Kernel
   → Encryption Policy
→ Encryption Framework
→ Storage Kernel
→ Storage

## Innovations

- Knowledge-aware encryption
- Policy-driven field encryption
- Per-tenant cryptographic isolation
- Pluggable crypto providers
- Provenance-aware encryption
- Event-safe encrypted payloads

## Limitations

V1 excludes encrypted vector indexes and encrypted graph traversal due to performance and research limitations.

## Implementation Roadmap

Phase 1:
- CryptoProvider
- Local KMS
- AES-GCM
- Page/WAL encryption

Phase 2:
- Envelope encryption
- Cloud KMS
- Tenant keys
- Online rotation

Phase 3:
- Field policies
- Object encryption
- Provenance encryption

Phase 4:
- Audit
- HSM
- Compliance tooling

Phase 5:
- Searchable encryption
- Secure enclaves
- PQC

## Acceptance Criteria

Functional:
- Page encryption
- WAL encryption
- Backup encryption
- Online key rotation

Security:
- No plaintext persisted
- AEAD validation
- Tamper detection

Performance:
- <10% write overhead
- Key lookup P95 <1ms

Reliability:
- Crash-safe rotation
- Encrypted recovery
- Backup restore

## Summary

Encryption is a dedicated architectural subsystem shared by the Knowledge Kernel and Storage Kernel. It provides layered protection, pluggable cryptography, key management, policy-based object encryption and a roadmap toward confidential knowledge computing.
