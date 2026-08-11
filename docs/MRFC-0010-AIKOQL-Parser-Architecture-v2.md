
# MRFC-0010: Aikoql Parser & Front-End Compiler Specification

**RFC ID:** MRFC-0010
**Version:** 2.0
**Status:** Draft
**Category:** Language & Compiler Architecture

---

# 1. Purpose

This RFC specifies the complete architecture of the aikoql parser and its position within the aikoql compiler pipeline.

The parser is **not** a standalone SQL parser. It is the first compiler stage of the Knowledge Computing Platform.

Its responsibility is to transform aikoql source into a deterministic Abstract Syntax Tree (AST), which is then progressively transformed into Knowledge Intermediate Representation (KIR), optimized, compiled into Knowledge Bytecode, and executed by the Knowledge Virtual Machine.

The parser SHALL NOT execute queries, resolve schemas, access storage, invoke AI services, or communicate with the Knowledge Kernel.

---

# 2. Architectural Position

```text
Applications
      │
REST / gRPC / SDK / MCP / aikoql
      │
API Layer
      │
┌─────────────────────────────────────────┐
│           Compiler Layer                │
│                                         │
│  Lexer                                 │
│    ↓                                   │
│  Parser   <----- THIS RFC              │
│    ↓                                   │
│  AST                                  │
│    ↓                                   │
│  Semantic Analyzer                     │
│    ↓                                   │
│  Knowledge Resolver                    │
│    ↓                                   │
│  Knowledge IR (KIR)                    │
│    ↓                                   │
│  Planner                              │
│    ↓                                   │
│  Optimizer                            │
│    ↓                                   │
│  Bytecode Generator                   │
└─────────────────────────────────────────┘
      │
Knowledge VM
      │
Knowledge Kernel
```

---

# 3. Compiler Responsibilities

## Lexer
- Tokenize source
- Unicode support
- Comments
- Numbers
- Strings
- Keywords
- Operators

Output:
Token Stream

## Parser
- Grammar validation
- AST construction
- Error recovery
- Source span preservation

Output:
Abstract Syntax Tree

## Semantic Analyzer
- Entity resolution
- Type checking
- Function resolution
- Relationship validation

Output:
Validated AST

## Knowledge Resolver
Maps language constructs to Knowledge Objects, Relationships and Views.

Output:
Knowledge IR

---

# 4. End-to-End Example

Input:

```aikoql
MATCH Person
WHERE company == "Visa"
AND city == "Amsterdam"
RETURN *
```

Lexer

```text
MATCH IDENT(Person)
WHERE IDENT(company)
EQ STRING("Visa")
AND IDENT(city)
EQ STRING("Amsterdam")
RETURN STAR EOF
```

Parser AST

```text
Query
 └── Match
      ├── Entity(Person)
      ├── Predicate
      │      company == "Visa"
      │      city == "Amsterdam"
      └── Projection(*)
```

Semantic Analyzer

- Resolve Person
- Resolve company
- Resolve city
- Verify operators

Knowledge IR

```text
Scan(Person)
 ↓
Filter(company="Visa")
 ↓
Filter(city="Amsterdam")
 ↓
Project(*)
```

Planner

Chooses:
- Object Scan
- Index Scan
- Parallel Scan

Optimizer

- Predicate pushdown
- Index selection
- Filter merge

Bytecode

```text
SCAN Person
FILTER company
FILTER city
PROJECT *
RETURN
```

Execution

Knowledge VM
→ Knowledge Kernel
→ Storage Kernel
→ Storage Engine

---

# 5. Hybrid Knowledge Query Example

```aikoql
MATCH Person
SIMILAR TO "John"
TRAVERSE managed_by
WHERE company == "Visa"
RETURN explain
```

Parser only produces syntax:

- MatchStatement
- SimilarityClause
- TraverseClause
- Predicate
- ExplainClause

Later stages map:

Similarity → Vector Service

Traverse → Graph Service

Predicate → Knowledge Kernel

Planner combines them into one hybrid execution plan.

---

# 6. Document Ingestion Example

```aikoql
INGEST "invoice.pdf"
EXTRACT tables
EXTRACT entities
BUILD relationships
COMMIT
```

Parser AST

```text
IngestStatement
 ├── Source(invoice.pdf)
 ├── ExtractTables
 ├── ExtractEntities
 ├── BuildRelationships
 └── Commit
```

Planner generates a workflow:

OCR
→ Layout Detection
→ Table Extraction
→ Entity Extraction
→ Relationship Discovery
→ Knowledge Object Builder
→ Commit

---

# 7. Natural Language Compilation

User:

"Find Visa engineers similar to John working on tokenization."

LLM Frontend

↓

aikoql

↓

Parser

↓

Compiler Pipeline

Parser is completely unaware that the original request was natural language.

---

# 8. Grammar Example (Simplified EBNF)

```ebnf
query = match | create | update | delete | ingest ;

match =
"MATCH" entity
["WHERE" predicate]
["SIMILAR" similarity]
["TRAVERSE" relation]
"RETURN" projection ;
```

---

# 9. Folder Structure

```text
compiler/parser/
├── lexer/
├── grammar/
├── ast/
├── visitor/
├── builder/
├── recovery/
├── diagnostics/
├── tests/
├── benches/
└── fuzz/
```

---

# 10. Diagnostics

Every parser error SHALL include:

- Stable error code
- Message
- Line
- Column
- Span
- Suggested fix

Example:

AIKOQL1004: Unexpected token '==' after WHERE.

---

# 11. Acceptance Criteria

Functional
- Parse all grammar rules.
- Deterministic AST.
- Error recovery.
- Preserve source spans.

Performance
- 100 KB query < 20 ms.
- Incremental reparse < 5 ms.

Quality
- 100% grammar coverage.
- Property tests.
- Fuzz tests.
- Golden AST snapshots.

---

# 12. Integration with System Architecture

The parser depends only on:

- Lexer
- Grammar
- Diagnostics

It outputs only:

- AST

It SHALL NEVER:

- Access Runtime
- Access Knowledge Kernel
- Access Storage
- Access Services
- Execute queries

This preserves the downward dependency model defined in MRFC-0005.

---

# 13. Future Extensions

- Natural-language frontend
- AI-assisted diagnostics
- User-defined functions
- Compile-time plugins
- Macro system

---

# 14. Summary

The aikoql parser is a pure compiler component. It converts aikoql syntax into an AST and hands it to the Semantic Analyzer. Every execution concern belongs to later compiler stages. This strict separation aligns with the layered architecture defined in MRFC-0005 and ensures a deterministic, testable, extensible compiler foundation.
