# Changelog

All notable changes to aikoql are recorded here, newest first.
Format: <https://keepachangelog.com/>-inspired, but terser. This file starts
at v0.1.19 (the last pre-launch tag); older history lives in the git tags
(`git log v0.1.0..v0.1.19`).

## [0.2.1] — 2026-10-03 (SDK wave + fuzz hardening)

The five-language SDK wave and the Layer B fuzz estate. No storage-format
or protocol change — a pure pin bump: the workspace, the protocol contract,
all five SDKs, the Claude Code plugin, and the npm package move in lockstep
(server and SDK refuse anything older than `0.2.1`).

### Added

- **Five first-party SDKs** (Phase D): Go, Python, Rust, TypeScript
  (all zero-dependency) and Java, sharing one cross-language conformance
  runner (16 identical vectors) and an eight-legged release-cert battery
  (`scripts/sdk-release-cert.sh`). Ships via PyPI / crates.io /
  npm / Maven Central / GitHub, release-cut automatically on tag
  (D-13..D-20).
- **SDK mutation harness** (D-17): twelve §29 mutants of the conformance
  truth, all killed by the shared runner.
- **Fuzz estate, Layer B** (F-05): seven cargo-fuzz decode boundaries over
  the real SDK wire logic (FZ-01..07), golden corpus, and a weekly
  `storage-fuzz` CI job.

### Changed

- The sfm004 pin-window flake in the storage snapshot matrix is hunted to
  a deterministic fix (F-06): the interleaved op now joins under the armed
  pin, making the pinned-segment assertion a real tooth.
- Workspace version `0.2.0` → `0.2.1` everywhere the contract touches:
  SDK `MIN_SERVER_VERSION`s, protocol `compatibility.json`, plugin,
  npm package, website and quickstart download pins.

## [0.2.0] — 2026-09-29 (launch)

The production launch: storage v2 as the default engine, hardened by the
L-11..L-27 launch milestone series (boundary matrices, proptest model
oracles, mutation-tested CI gates, a strong-claims registry, and a
per-site security disposition).

### Added

- **Storage v2 default.** The segmented LSM engine (WAL + memtable +
  segments + directory checkpoints) is the production default, ratified
  by ADR (SE2-M41, 2026-09-07). Fresh paths auto-create `aikoql-v2`;
  write-mixed 6.0× / p99 33× vs redb, RSS −30%, disk −65%, bounded
  crash recovery.
- **Durability by default.** Every ack rides an fsynced WAL frame
  (`Sync` mode); acked writes survive a hard-killed server at both the
  kernel boundary (durability suite) and the MCP boundary
  (`m_abrupt_kill_preserves_acked_writes`).
- **Background compaction** with group commit and backpressure; directory
  checkpoints (`CHECKPOINT-{gen}.log`) with fail-closed damage handling.
- **HNSW vector index** and secondary indexes (P5-M18..M21).
- **MCP server** (`aikoql-mcp serve`, stdio + TCP with fail-closed
  token auth) and the Go + Python client SDKs with a pinned minimum
  server version contract.
- **Launch hardening**: boundary/length matrices (two real product bugs
  found and fixed), proptest model oracles over every storage layer,
  a strong-claims registry pinning the estate's performance claims to
  evidence tests, and mutation-tested CI gates (ten mutation classes,
  all caught).
- `docs/SECURITY-DISPOSITION.md` — per-site disposition of the code
  scanning and Dependabot findings.

### Security

- Fixed the `lru` advisory (GHSA-rhfx-m35p-ff5j): bumped the StarRocks
  harness dev-dependency `mysql` 26 → 28 (lru 0.12.5 → 0.18.5); the
  vulnerable `IterMut` API was never called by the in-tree usage.
- Disposed all 28 code-scanning alerts (zero real findings) and
  resolved the R2-008 CodeQL threads.

### Honest boundary

Single-node; MCP-only wire; ops tooling stops at backup/restore; filtered
scan ~4.3× vs PostgreSQL (certified bound ≤8×, roadmap target ≤2×); no
conversation→knowledge ingestion loop. All post-launch, evidence-gated.

## [0.1.19] — 2026-09-07

The storage-v2 era (SE2): WAL + memtable + segmented LSM built and
certified (SE2-M1..M40), the v2 default ratified in SE2-M41, release
identity verification, and the competitor benchmark published.
