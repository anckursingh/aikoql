# AIKOQL

**Persistent, progressive memory for AI agents.** AIKOQL is a knowledge
database your agents write to and read from — every fact stays durable,
versioned, and queryable across sessions, so an agent no longer re-reads
the whole context on every turn. Measured on our own repository
(aikoql 0.4.0): an agent
pulling aikoql recall slices instead of re-reading the corpus reads
**99.95% fewer tokens** (baseline 14,026,488 → memory 6,538 cl100k tokens
over 3 task queries; corpus 4,675,496 tokens — see
[benchmarks/results/token-savings.json](benchmarks/results/token-savings.json)
for the harness, the binary provenance, and the full record).

**Try it in two minutes** — no binary install:

```bash
AIKOQL_ADMIN_PASSWORD=$(openssl rand -hex 12) \
  docker compose -f deploy/docker-compose.yml up -d
docker compose -f deploy/docker-compose.yml logs tunnel   # prints your URL
```

Open the URL as a remote MCP server in claude.ai — the connector is
`https://<url>/mcp` and the built-in OAuth sign-in takes care of
credentials. Or connect any MCP client. The full walkthrough:
[QUICKSTART.md](QUICKSTART.md).

- **Encrypted at rest** — journal, index, and artifacts under built-in
  encryption and key management (KMS).
- **Hybrid vector + text search** — RRF-fused recall with per-model
  shards, filtered ANN, and product quantization.
- **MCP-first** — stdio or Streamable HTTP, with first-party SDKs for
  Go, Java, Python, Rust, and TypeScript.
- **Built for agents** — agent memory, recall slices under a token
  budget, facts with provenance, contradiction tracking, time travel.

AIKOQL is **AGPL-3.0-only** open source: free to use, modify, and run
commercially; if you distribute modified versions, release your changes
under the same license. The community edition is the full server;
enterprise tiers (on-prem or managed on S3/MinIO/Cloudflare R2) add
SSO, RBAC, audit logs, and support SLAs.

## Enterprise tier

Same binary, licensed features. Without a valid key the server runs as the
community edition — nothing is taken away.

- **License** — offline-verified ed25519 key at `.aikoql-license` in the
  data dir, or a path from `AIKOQL_LICENSE`. An invalid/expired key degrades
  to community mode with a warning (72h expiry grace). Features:
  `oidc-sso` (implies `multi-user`), `multi-user`, `audit-export`.
- **SSO and roles** — OIDC SSO on both the Studio login and the MCP OAuth
  sign-in (`AIKOQL_OIDC_ISSUER`, `AIKOQL_OIDC_CLIENT_ID`,
  `AIKOQL_OIDC_CLIENT_SECRET`, `AIKOQL_OIDC_REDIRECT_URI`,
  `AIKOQL_OIDC_ROLES_CLAIM`). Roles: `admin`, `viewer` (read-only), looked
  up live on every request. `AIKOQL_ADMIN_PASSWORD` seeds the bootstrap
  break-glass admin on first start only.
- **Audit export** — retention rotation (`AIKOQL_AUDIT_RETENTION_DAYS`,
  default 90) plus JSONL export via `aikoql-mcp audit --export-jsonl PATH`
  and the Studio admin panel.
- **Replicated deployments** — users and roles live in the database's
  replicated store, so multi-user runs on a leader + standby or HA pair.

Full usage doc — key issuance, the admin API, ops notes:
[docs/ENTERPRISE-TIERS.md](docs/ENTERPRISE-TIERS.md).

- **Quickstart** — install, start, ingest, query, add sources, build a
  knowledge-backed agent, debug: [QUICKSTART.md](QUICKSTART.md)
- **Examples** — the seven-step hello-agent flow in runnable form:
  [examples/](examples/)
- **Design docs** — architecture, query language, agent interface:
  [docs/](docs/)
- **Releases** — pinned binaries and container images:
  https://github.com/anckursingh/aikoql/releases
