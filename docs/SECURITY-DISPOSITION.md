# Security disposition (L-25)

Date: 2026-09-29 · branch `feature/aikoql-db-launch` · repo `anckursingh/aikoql`

Scope: the launch review's open security items — 28 open code-scanning
alerts, the R2-008 CodeQL threads, and the P1-8 kernel-write-loss follow-up.
Method: every alert site was read in the working tree (the remote alert
locations are pinned to the scanned head; where the local line drifted the
flagged construct was located and named). Disposition per site: `fix real`
(change the code), `by-design` (the behavior is deliberate and commented),
or `false positive` (the sink is mislabeled — no secret reaches a log, no
hard-coded value is used as key material).

Verdict: **zero real findings** in the 28 alerts. One real guarantee gap
(the P1-8 kernel leg) is now pinned by a test with a captured RED.

## 1. The 28 code-scanning alerts

### 1.1 `rust/hard-coded-cryptographic-value` (20 alerts)

The rule flags literals in cryptographic call positions. In every flagged
production site the "hard-coded value" is the **zero-initializer of a
buffer that a CSPRNG fills before any use** — the literal is never the
material. Test sites carry fixed vectors and dev-only credentials, which is
their job.

| alert | site (scanned head) | local construct | disposition |
|---|---|---|---|
| 196 | `crates/kernel/src/security/kms.rs:192` | `let mut nonce_bytes = [0u8; NONCE_LEN]; rand::thread_rng().fill_bytes(...)` | false positive — zero-init buffer, CSPRNG-filled before use |
| 195 | `crates/kernel/src/security/crypto.rs:206` | same pattern (`ChaCha20Poly1305::encrypt`) | false positive — zero-init buffer, CSPRNG-filled before use |
| 194 | `crates/kernel/src/security/crypto.rs:110` | same pattern (`Aes256Gcm::encrypt`) | false positive — zero-init buffer, CSPRNG-filled before use |
| 182 | `crates/kernel/src/security/hkdf.rs:38` | `t.update(&[0x01])` — the HKDF counter octet | false positive — RFC 5869 §2.3 counter byte, not key material |
| 170 | `crates/kernel/src/security/kms.rs:292` | `let mut dk = [0u8; 32]; argon.hash_password_into(...)` | false positive — zero-init output buffer, argon2 fills it |
| 169 | `crates/kernel/src/security/crypto.rs:250` | `let mut key = [0u8; 32]; rand::thread_rng().fill_bytes(...)` | false positive — zero-init buffer, CSPRNG-filled before use |
| 168 | `crates/kernel/src/security/crypto.rs:154` | same pattern (`Aes256Gcm::generate_key`) | false positive — zero-init buffer, CSPRNG-filled before use |
| 103 | `crates/services/api/mcp/src/cli.rs:288` | `let mut password = "password";` — the `import neo4j` flag default, overridable via `--password` | by-design — documented dev default for local neo4j imports (drifted line: cli.rs:325) |
| 180 | `crates/services/api/mcp/tests/connectors/mod.rs:125` | `AIKOQL_TEST_NEO4J_PASSWORD` env or fallback `"password"` — integration-test connector credentials | by-design — test-only credentials for local dev containers |
| 179/178, 46..38 | `crates/kernel/tests/durability.rs` (10 alerts: 179@371, 178@67, 46@329, 45@297, 44@245, 43@209, 42@159, 41@89, 40@82, 38@57) | fixed salt vectors in the durability suite's fixtures | false positive — test vectors; no production key path |

### 1.2 `rust/cleartext-logging` (8 alerts)

| alert | site (scanned head) | local construct | disposition |
|---|---|---|---|
| 172 | `crates/services/api/mcp/src/admin.rs:165` | `println!("generated passphrase (save it): {p}")` in `run_keygen` — the one-time passphrase print, commented: "the key file is useless without it, so keep a copy" | by-design — deliberate one-time print at generation |
| 173 | `crates/services/api/mcp/src/shell.rs:283` | `println!("Related: {} -> {} ...")` — KOIDs printed by the interactive shell | by-design — KOIDs are public identifiers, not secrets |
| 175 | `crates/ingestion/tests/multimodal_golden.rs:198` | `eprintln!` of extraction metrics in the golden fixture test | false positive — test-only metric diagnostics |
| 174 | `crates/ingestion/tests/generate_multimodal_fixtures.rs:384` | `eprintln!` of fixture-generation metrics | false positive — test-only metric diagnostics |
| 166/165/164 | `crates/ingestion/src/secret_filter.rs:783/803/824` | `assert!(findings.is_empty(), "...{:?}", findings)` — test assertions formatting the filter's **redacted** output | false positive — the logged value is the filter's own findings, not a secret |
| 171 | `crates/kernel/tests/qa2_knowledge.rs:277` | a `remember_trusted(...)` construction in a test | false positive — test code |
| 167 | `crates/kernel/tests/conformance.rs:1001` | `let secret = create_with_vec(...)` — a test variable named `secret` feeding a similarity assertion | false positive — variable naming; nothing logged |

## 2. R2-008 CodeQL threads

The 8 CodeQL bot threads on PR #6 (alerts 188, 189, 190, 197, 198, 201,
203, 204) are all `isResolved: true` on GitHub (verified via the GraphQL
review-thread listing, 2026-09-29) — the two threads on `crates/runtime/tests`
carry the disposition reply citing `docs/PR6-TDD-DISPOSITIONS.md` (R2-008).
Nothing left to resolve.

## 3. P1-8 kernel leg — durable-by-default flush

The P1-8 dogfood trap: the kernel lost writes on abrupt MCP-server
termination. Verdict on the current head: the guarantee **holds** — the
engine's `DurabilityMode` default is `Sync`, so every ack rides an fsynced
WAL frame (`db.rs` write path), and `durability.rs` d04/d05 already prove
abrupt termination at the kernel boundary. What was missing was the pin at
the **MCP boundary**, which is now in place:

- `mcp_stdio.rs::m_abrupt_kill_preserves_acked_writes` (commit `0b3c126`):
  acked `remember` writes survive a hard-killed server
  (TerminateProcess/SIGKILL — no stdin EOF, no `Db::drop`, no maintainer
  checkpoint) across a reopen. GREEN against the estate (pin-only).
- Teeth proof archived: `docs/red-archive/abrupt-close-vs-async-durability`
  (exit 101) — with the WAL append removed from the write path (an ack
  riding the memtable alone, the exact loss class the pin guards), the same
  test REDs. The mutation was uncommitted and reverted.
- Honest limits: an fsync-less-but-WAL-append write (the `Async` mode)
  survives a *process* kill via page cache — `Async` only loses on power
  loss. The pin guards the ack-durability contract at the MCP boundary, not
  power-loss semantics.

## 4. Dependabot — `lru` advisory (L-26 addendum)

Alert #2 (`GHSA-rhfx-m35p-ff5j`, `lru` `IterMut` Stacked-Borrows UB, severity
low, vulnerable `< 0.16.3`): **fixed** — not just investigated. The only
path pulling the vulnerable `lru` 0.12.5 into the graph was the StarRocks
harness dev-dependency `mysql` 26 (a test-only adapter, and its only `lru`
calls were `LruCache::pop_lru` — the advisory's `IterMut` API was never
called). `mysql` bumped 26 → 28 (Cargo.lock: lru 0.12.5 → 0.18.5), the
graph now carries only lru 0.16.4 (tantivy) and 0.18.5, both patched. The
alert auto-resolves on GitHub when this lands.

## 5. Open items carried forward

- The 28 alerts remain `open` on GitHub — this doc is the disposition
  record; dismissal on GitHub (with reason per site) is a separate
  outward action if wanted.
- `never/must/always` claim vocabulary and the R2-008 "no in-source
  suppression for Rust" constraint: CodeQL noise will re-accumulate as new
  sites are added; the L-23 strong-claims sweep does not cover it (different
  vocabulary).
