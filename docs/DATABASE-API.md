# Database API — the canonical SDK contract

Phase 0 freeze (D-01, D-02) of the PR #7 multi-language SDK review
(§4, §12). `protocol/api-v1.json` and `protocol/errors.json` are the
machine-readable pins; this document is the prose contract, and
`scripts/check-sdk-compat.sh` fails on any drift between the two.

Contract version: **api_major 1**. Changing the lifecycle, the operation
set, or the error taxonomy is an api_major bump — never a silent edit.

## Lifecycle (§4.1)

| type | what it is |
|---|---|
| Database | the handle to one database: open, config, top-level admin |
| Connection | one transport session to a server: dial, authenticate, close |
| Session | scoped state on a connection: tenant, context, cancellation |
| Transaction | an atomic unit on a session: begin / commit / rollback |
| Statement | a compiled or prepared statement: bind, execute |
| Result | one operation's outcome: status, ids, counts, rows |
| Row | one record with typed field access |

## Operations (§4.2)

| operation | semantics |
|---|---|
| connect | open a connection to a server (TCP or stdio); returns a Connection |
| close | release the connection and every child resource |
| ping | liveness probe; must not require initialization |
| health | structured health: ready flag, version, queue depths |
| remember | persist a knowledge object; returns its KOID |
| get | read one object by KOID |
| update | replace an object (version-checked) |
| delete | tombstone or erase an object |
| execute | run a statement; returns a Result |
| query | run a query; returns a ResultSet |
| prepare | compile a statement once; returns a Statement |
| begin | open a transaction; returns a Transaction |
| commit | commit the open transaction |
| rollback | abort the open transaction |
| batch | atomic multi-operation batch |
| find_similar | vector / hybrid recall |
| relate | add a directed edge between objects |
| traverse | walk edges from a KOID |
| create_schema | register a type schema |
| discover_schema | introspect the registered types |
| create_index | declare and refresh an index |
| drop_index | remove an index |
| explain | query plan without execution |
| trace | lineage of a KOID (versions + events) |
| prove | audit-chain verification |
| backup | snapshot the store |
| restore | restore from a backup |
| metrics | server metrics |

## Streaming (§4.3)

Every SDK defines `query -> ResultSet` and
`query_stream -> Stream<Result>` with cancellation. Streaming is part of
the canonical API, not an SDK-specific extension.

## Error model (SDK-012)

The frozen taxonomy lives in `protocol/errors.json`: every error carries
`code`, `message`, `retryable`, `suggestion`, `request_id`. SDKs map
protocol errors to their idiomatic exception type **while preserving all
five fields** — a mapped error must never lose its code. The code table
lands with D-02.
