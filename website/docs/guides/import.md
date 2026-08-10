---
title: Import Data
description: Import from PostgreSQL, SQLite, MongoDB, Neo4j
---

# Cross-DB Import

Mnemosyne ships with 4 database connectors for importing existing data.

## PostgreSQL

```bash
mnemosyne import postgres 'host=localhost user=postgres dbname=hr_db' --tenant acme

# Specific table only
mnemosyne import postgres 'host=localhost dbname=prod' --table employees
```

**Mapping:** Table → type_name, PK → deterministic KOID, columns → PropertyMap. PG types mapped: integer→Int, text→Text, boolean→Bool, json→Text, timestamp→Text.

## SQLite

```bash
mnemosyne import sqlite ./employees.db --tenant acme

# Specific table
mnemosyne import sqlite ./app.db --table users
```

**Mapping:** Same as PostgreSQL. SQLite types: INTEGER→Int, REAL→Float, TEXT→Text, BLOB→Bytes.

## MongoDB

```bash
mnemosyne import mongodb mongodb://localhost:27017 --db hr_app --tenant acme

# Specific collection
mnemosyne import mongodb mongodb://localhost:27017 --db prod --collection users
```

**Mapping:** Collection → type_name, `_id` (ObjectId) → hex string → deterministic KOID. BSON types: Double→Float, String→Text, Int32/Int64→Int, Bool→Bool, Document→Map, Array→List, Binary→Bytes, ObjectId→Text(hex).

Nested documents are flattened with dot-notation (`address.city`).

## Neo4j

```bash
mnemosyne import neo4j http://localhost:7474 --user neo4j --password secret

# Specific label
mnemosyne import neo4j http://localhost:7474 --label Person
```

**Two-phase import:**
1. Nodes → KnowledgeObjects (elementId → KOID)
2. Relationships → RelationshipRef (using elementId→KOID map)

## Import Architecture

```
Source DB → Connector → Schema Discovery → Row/Node Mapping → KO Creation → Mnemosyne
```

All imports are:
- **Deterministic** — same source row → same KOID every time
- **Idempotent** — re-running the import doesn't create duplicates
- **Tenant-aware** — tag all imported objects with a tenant
- **Auditable** — each import creates a KnowledgeEvent

## Verifying Imports

After import, use the shell or graph browser to verify:

```bash
mnemosyne shell ./kb.redb

Mnemosyne> .tables
  employees
  projects

Mnemosyne> MATCH employees RETURN *
── 1042 rows ──
```
