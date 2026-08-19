---
title: Encryption
description: AES-256-GCM encryption at rest, envelope encryption, field-level policies
---

# Encryption Guide

aikoql provides encryption at rest with AES-256-GCM and ChaCha20-Poly1305, envelope encryption (KEK → DEK → Data), whole-store encryption, and field-level encryption policies — wired end-to-end into `serve` (v0.2).

## Quick Start

```bash
# 1. Generate a master key (passphrase from AIKOQL_PASSPHRASE, or generated + printed once)
export AIKOQL_PASSPHRASE="your-secure-passphrase"
aikoql keygen ./aikoql.key
# → writes the v2 key envelope (88 bytes). Restrict permissions: chmod 600 ./aikoql.key

# 2. Configure aikoql.toml
cat > aikoql.toml <<'EOF'
[encryption]
enabled = true
key_path = "./aikoql.key"

[encryption.policies]
employee = ["salary", "ssn"]
EOF

# 3. Start — the passphrase comes from the env (env beats TOML)
aikoql serve --metrics-addr :9091 ./encrypted.redb
```

`enabled = true` with no reachable passphrase (no env, no TOML) fails closed — aikoql refuses to open the database in silent plaintext mode.

## Configuration

```toml
[encryption]
enabled = true
key_path = "./aikoql.key"          # default: ./aikoql.key
passphrase = "..."                 # optional — AIKOQL_PASSPHRASE env wins if both set

[encryption.policies]
employee = ["salary", "ssn"]       # type_name → fields encrypted on remember
```

- `key_path` — the KEK (master key) file written by `aikoql keygen`.
- `passphrase` — either in TOML or `AIKOQL_PASSPHRASE`; the environment variable takes precedence.
- `[encryption.policies]` — per-type field lists. Fields listed are stored as ciphertext with the field name as AAD; everything else stays plaintext.

Wrong passphrase on open → `InvalidPassphrase` and the server exits. There is no plaintext fallback.

## Key Hierarchy

```
Key Encryption Key (KEK)          — aikoql.key (v2 envelope, 88 bytes)
    ↓ wraps
Data Encryption Keys (DEKs)       — per-tenant, wrapped by KEK, persisted in the store
    ↓ encrypt
Field ciphertext                  — version || nonce || ciphertext || tag
```

- The KEK wraps per-tenant Data Encryption Keys (DEKs); each tenant gets a unique DEK for key isolation.
- Wrapped DEKs are **persisted inside the encrypted store**, so field-encrypted data survives restarts — a fresh boot decrypts with the same KEK + passphrase.
- `aikoql keygen` writes the v2 envelope. Legacy v1 key files (48 bytes) auto-migrate on first use.
- Key audit events (creation, usage, failure) are logged to an append-only audit log.

## Field-Level Encryption

Mark specific properties as encrypted per schema type in `aikoql.toml`:

```toml
[encryption.policies]
employee = ["salary", "ssn"]
```

Remembered objects of type `employee` store `salary` and `ssn` as AES-256-GCM ciphertext (field name as AAD); `name` and other fields stay plaintext. The whole store is additionally wrapped by the encrypted store engine.

## Compliance

```bash
# Encryption compliance report
curl http://localhost:9091/api/v1/compliance \
  -H 'Authorization: Bearer TOKEN'

# or via MCP
{"method":"tools/call","params":{"name":"compliance_report","arguments":{}}}
```

Returns: encryption status, policies registered, key inventory (KEK + wrapped DEK count), audit event breakdown, compliance grade (A/C).

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

## Scope Notes

- **Online KEK rotation is not part of v0.2** — rotation would require a full-store re-encrypt and has no production caller yet. DEK wrapping/unwrapping at open is fully supported.
- `aikoql keygen -` prints the passphrase only (useful for CI secret injection).
