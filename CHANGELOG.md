# Changelog

All notable changes to aikoql are recorded here, newest first.
Format: <https://keepachangelog.com/>-inspired, but terser. This file starts
at v0.1.19 (the last pre-launch tag); older history lives in the git tags
(`git log v0.1.0..v0.1.19`).

## [Unreleased]

## [0.2.4] — 2026-10-30 (remote MCP: HTTP, OAuth, TLS)

Upgrading from 0.2.3 (Docker): agent memory written by 0.2.3 lives inside
the container (`/memory`), outside the volume — copy it into the volume
before recreating: `docker exec aikoql sh -c 'mkdir -p /data/memory && cp -a
/memory/. /data/memory/'`. The database directory is renamed on the first
start of the new image; nothing else to do. Set
`-e AIKOQL_MODEL_AUTO_INSTALL=1` to have semantic search install its model.

### Added

- **TLS on every client-facing listener**: `--tls-cert/--tls-key` (or
  `AIKOQL_TLS_CERT` / `AIKOQL_TLS_KEY`) put TLS on `--listen`,
  `--native-port` and the HTTP surface (`--metrics-addr`, served as
  HTTPS). With TLS a client listener may bind any interface; without it
  plaintext stays loopback-only.
- **TLS in all five SDKs**: Rust `Client::dial_tls` / `connect_native_tls`
  with `Tls::with_ca` / `Tls::public`, Python `McpClient(tls=SSLContext)`,
  Go `WithTLS(*tls.Config)`, TypeScript `Client.dial(addr, { tls })`, Java
  `AikoqlClient.dial(addr, SSLContext)` — the server's certificate is
  verified before the token is sent; a NOT_LEADER re-dial keeps TLS.
- **MCP over Streamable HTTP**: `POST /mcp` on the HTTP surface —
  bearer-token auth (the `--tcp-token` tokens), `Mcp-Session-Id` sessions
  bound to their token, `DELETE` to end one, a loopback-only `Origin` guard
  (`AIKOQL_MCP_ALLOWED_ORIGINS` to extend it), 1 MiB bodies; HTTPS with
  `--tls-cert`. For remote MCP clients (`claude mcp add --transport http`).
  `GET /mcp` is the session's event stream (subscription events, a streamed
  query's later chunks; a newer GET supersedes; `Last-Event-ID` resumes).
- **OAuth 2.1 for MCP over HTTP** — claude.ai connectors can sign in: with
  `[auth]` logins the server is its own authorization server (RFC 9728 /
  8414 discovery from /mcp's 401, RFC 7591 registration of public clients,
  authorization code + S256 PKCE behind a login/consent page, rotating
  refresh tokens, audience-bound opaque tokens). Sign-ins persist in the
  database (SHA-256-keyed under reserved `sys/oauth/` keys: restarts,
  failovers, encryption and backups included; a removed login is revoked);
  `AIKOQL_PUBLIC_URL` sets the advertised URL behind a proxy. Verified
  against claude.ai itself (sign-in, remember/get, semantic search).
- **Client ID metadata documents (CIMD)**: an https `client_id` (Claude
  Code's) is resolved by fetching its document — public addresses only
  (checked in the resolver the connection uses), no redirects, short
  timeouts, 64 KB, cached 5 min; the document must name itself and be a
  public client. Advertised as `client_id_metadata_document_supported`.
- **`AIKOQL_MODEL_AUTO_INSTALL=1`** (or `[embedding] auto_install = true`): `serve` downloads the default embedding
  model once when it is missing (in Docker: into the `/data/models` volume);
  a failed download still serves, and `/health` says why. Off by default —
  the runtime still never downloads unasked. Model downloads honour
  `HF_ENDPOINT` and `HF_HOME`.
- **Sign-in backoff**: after five wrong passwords a login waits 1 s, then
  each failure doubles it (up to 15 min) — even the right password is
  refused meanwhile: 429 on `/api/login`, 429 + `Retry-After` on the OAuth
  sign-in page. A success resets it; unknown names are not tracked.
- **MCP protocol version negotiation**: `initialize` answers the client's
  requested version when supported (2024-11-05, 2025-03-26, 2025-06-18),
  else the newest; it used to answer 2024-11-05 to everyone. Over HTTP an
  unsupported `MCP-Protocol-Version` header is 400.
- **Docker publishes over TLS**: with a mounted certificate and
  `AIKOQL_TLS_CERT` / `AIKOQL_TLS_KEY`, the image listens on 0.0.0.0:9090
  (`-p 9090:9090` works); 9091 too when armed with
  `AIKOQL_ALLOW_REMOTE_HTTP` and a login.

### Changed

- **Breaking — remote HTTP needs TLS**: a non-loopback `--metrics-addr`
  with `allow_remote_http = true` and credentials used to serve logins and
  session tokens in plaintext; it is now refused (exit 2) unless
  `--tls-cert/--tls-key` are set. Loopback HTTP is unchanged.
- **Docker**: the database lives at `/data/aikoql` (was the redb-era name
  `/data/aikoql.redb`; a volume from an image up to 0.2.3 is renamed once,
  in place, on first start) and agent memory at `/data/memory`
  (`AIKOQL_MEMORY_DIR`; it used to live outside the volume and was lost on
  recreate).

### Fixed

- **The OAuth sign-in stayed on its page in Chrome**: the page's
  `form-action 'self'` blocked the redirect back to the client after a
  correct login; it now allows the (registered) redirect's origin.
- **Redirect URIs are screened**: a line break, space, `;` or a bad port in
  a registered redirect URI is refused (it is written into the `Location`
  header).
- **Headers split across reads**: the HTTP surface took its headers from
  one read, so a request whose headers arrived in two pieces (a slow
  client, or headers past one TCP segment) lost `Content-Length` and its
  body. It now reads to the end of the headers.
- **`aikoql model install --model-dir DIR`** without a model id ignored the
  directory and installed into `~/.aikoql/models`.

## [0.2.3] — 2026-10-17 (training data + POC hardening)

Recorded after the fact (2026-10-30) from the tag's history — 0.2.3 shipped
without a changelog section.

### Added

- **Training-data pipeline** (PR #9, `training/`): canonical schema,
  scenario generators (temporal, provenance, uncertainty, conflict,
  authorization), grounded answers with claim-level grounding, dataset
  validator and gates, synthetic multi-domain corpora with held-out
  organizations, fine-tune run + per-capability scorecard, the chat
  wrapper; `training-data.yml` workflow.
- **Query**: `CHANGESET BETWEEN` (change detection over generation
  windows), `AS_OF JOURNAL <n>`, `WHERE a OR b` returns the union, GROUP BY
  count through MCP tools; AS_OF/HISTORICAL reconstruct retraction history.
- **Studio** (POC-4 S-1..S-13): epistemic graph styling, provenance and
  timeline panels, ontology hot-reload, clearer AIKOQL1030 hints.

### Changed

- **Tenant isolation is strict**: scoped reads deny tenantless rows,
  unscoped reads are confined, empty tenants are rejected (ACCESS_DENIED);
  idempotency keys are namespaced by tenant and op kind.
- **Supersede**: atomic successor pairs at one HLC instant, successors
  inherit tenant/scope/authority and honour `valid_from`; batch supersede.
- `serve` with only `--metrics-addr` parks instead of exiting on stdin EOF.

## [0.2.2] — 2026-10-03 (Java namespace)

Recorded after the fact (2026-10-30) — 0.2.2 shipped without a changelog
section.

### Changed

- **Breaking for Java**: the Maven groupId and package moved from
  `io.aikoql` to `com.aikoql` (`com.aikoql:aikoql-client`,
  `com.aikoql.client`) — the namespace must reverse to a domain the
  project owns.

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
