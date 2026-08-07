---
title: Encryption
description: AES-256-GCM encryption at rest, envelope encryption, field-level policies
---

# Encryption Guide

Mnemosyne provides encryption at rest with AES-256-GCM and ChaCha20-Poly1305, envelope encryption (KEK → DEK → Data), online key rotation, and field-level encryption policies.

## Quick Start

```bash
# Generate a master key
mnemosyne keygen ./master.key

# Start with encryption
MNEMOSYNE_PASSPHRASE="your-passphrase" mnemosyne serve --metrics-addr :9091 ./encrypted.redb
```

## Architecture

```
Application Encryption (optional)
    ↓
Knowledge Encryption (field/object level)
    ↓
Storage Encryption (page/WAL level)
    ↓
Disk Encryption (OS-provided)
```

**Key Hierarchy:**
```
Root (KMS/HSM) → Master → Tenant → Database → Object → Field
```

**Envelope Encryption:**
- Key Encryption Key (KEK) wraps per-tenant Data Encryption Keys (DEKs)
- KEK rotation rewraps DEKs — no data re-encryption needed
- Each tenant gets a unique DEK for key isolation

## Field-Level Encryption

Mark specific properties as encrypted per schema type:

```bash
# Register an encryption policy
curl -X POST http://localhost:9091/api/v1/remember \
  -d '{"type_name":"Employee","properties":{"salary":"encrypted","ssn":"encrypted","name":"plaintext"}}'
```

Properties marked as encrypted are stored as AES-256-GCM ciphertext with the field name as AAD.

## Key Rotation

Online rotation without downtime:

```bash
# Rotate KEK — rewraps all DEKs automatically
# (via MCP or scheduled KeyRotationJob)
```

Key audit events (creation, rotation, usage, failure) are logged to an append-only audit log.

## Compliance

```bash
# Encryption compliance report
curl http://localhost:9091/api/v1/compliance \
  -H 'Authorization: Bearer TOKEN'
```

Returns: encryption status, policies registered, tenant key count, audit event breakdown, compliance grade (A/C).

## Algorithms

| Algorithm | Version Byte | Use Case |
|---|---|---|
| AES-256-GCM | 0x01 | Primary cipher |
| ChaCha20-Poly1305 | 0x02 | Secondary (crypto agility) |

Both use 12-byte nonce, 16-byte authentication tag, 256-bit key. Cipher-cached per-key for performance (16.6% write overhead vs plaintext).

## Page Format

```
version_byte(1) || nonce(12) || ciphertext || authentication_tag(16)
```

Key-as-AAD binding prevents key-swapping attacks.
