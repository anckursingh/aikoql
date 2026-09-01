# Corruption Handling

Date: 2026-09-01 · engine: AikoqlStorageEngine (redb/RocksDB vendor-owned, NOT_MEASURED)

§31 canonical report. Aggregates the three phases that cover corruption
of the durable store: the record envelope (KSE-3, §9), crash-consistency
fault injection (KSE-9, §15), and derived-index repair (KSE-10, §16).
MemoryEngine has no persistence, so it has no corruption surface.

## Envelope-level damage (KSE-020..023, `kse3_envelope.rs` + `src/envelope.rs`)

| gate | damage shape | result |
|---|---|---|
| KSE-021 | one flipped payload bit (reopen after tamper) | deterministic corruption error — never served as valid data |
| KSE-021 (in-memory) | corrupt payload, bad checksum, truncation, bad version | envelope decode fails closed (7 unit tests) |
| KSE-022 | torn tail (crash mid-append cuts the record short) | truncates to the last complete record boundary and opens |
| KSE-023 | foreign file at the path | refuses to open — no accidental interpretation of another format |

Torn-tail and corruption are distinguished: a truncated record is a normal
crash artifact and is healed; a damaged complete record is an error. One
platform fix surfaced: Windows cannot `SetEndOfFile` through an
append-mode handle, so torn-tail truncation uses a transient plain-write
handle.

## WAL fault injection (KSE-080..083, `kse9_crash_consistency.rs`)

Seed shaped so `ko/`/`relo`/`reli` rows are atoms of one checksummed
record. Fault classes map onto the WAL's real crash surface:

| gate | fault | result |
|---|---|---|
| KSE-080 | crash before commit (last record truncated) | pre-batch state — no partial rows, no phantom edges |
| KSE-081 | crash after commit | committed state survives restart exactly |
| KSE-082 | byte-flip inside a complete record | fails closed AND leaves the file untouched — index divergence is unreachable by construction (one record = one batch = one checksum) |
| KSE-083 | recovery | truncation restores the last good state; corruption errors, never silent wrong data |

## Corrupt derived index (KSE-092, `kse10_index_rebuild.rs`)

Derived indexes (`relo`/`reli`/`type`) are WAL rows — they can be damaged
by the same bitrot as anything else. `Kernel::rebuild_derived_indexes()`
treats canonical `ko/` heads as the authority and applies one atomic
disjoint-diff batch:

| damage | before rebuild | after rebuild |
|---|---|---|
| malformed `relo` key planted | outbound_edges fails closed (detection, never silent wrong data) | key swept — removed_invalid=1 |
| stale ghost edge planted | honestly visible (no silent filtering) | swept — removed_stale=1; final set byte-exact |

Identical outcomes over MemoryEngine (reference), redb, and
AikoqlStorageEngine.

## Honest limits

- redb / RocksDB corruption recovery is vendor-owned — NOT_MEASURED here
  (the WAL surface is aikoql-specific).
- KSE-082 pins single-record damage. Multi-record damage with a valid
  tail (e.g. two complete records bit-rotted, no truncation) is caught by
  the same checksum rule — every record is individually checksummed — but
  the specific two-record scenario is not a named pin.
