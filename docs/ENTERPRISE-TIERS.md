# Enterprise tier — aikoql

One binary, two tiers. The community edition (free, AGPL-3.0-only) keeps
everything shipped today. The enterprise tier unlocks features behind an
**offline-verified license key**: OIDC SSO, a multi-user store with roles,
and audit retention + export. There is no phone-home; an air-gapped
deployment works. The managed tier (cloud.aikoql.com) runs this same
enterprise edition.

## The license key

- **Format**: JSON payload + ed25519 signature, base64url-encoded, no
  padding. Payload fields: `ver` (1), `org`, `issued_at` (unix seconds),
  `expires_at` (owner's `YYYY-MM-DD`, optional — omitted means perpetual),
  `features` (array).
- **Where it lives**: `.aikoql-license` in the data dir, or a path from the
  env `AIKOQL_LICENSE` (env wins).
- **Verification**: fully offline at startup. The public key is embedded in
  the binary at build time — release builds need no secrets.
- **Failure behavior**: an invalid or expired key never bricks the server —
  it **still runs in community mode** with a logged warning, and enterprise
  features are disabled. Expiry gets a **72h grace window** against clock
  skew.
- **Features**: `oidc-sso` (implies `multi-user`), `multi-user`,
  `audit-export` (covers both retention and export); `"all"` expands to all
  three.

## Issuing keys (owner-side)

`scripts/license-keygen/` is the owner's signing tool — a standalone crate
that is **never shipped** in release binaries and never sees CI.

```bash
cd scripts/license-keygen && cargo run --release -- keygen
# prints SK=<private key> and PK=<public key>

AIKOQL_LICENSE_SK=<the SK= line> \
  cargo run --release -- sign --org "Acme Corp" [--expires YYYY-MM-DD] \
  --features all|oidc-sso,multi-user,audit-export
```

Custody rules: the public key is committed as a constant in the source; the
private key lives only in the owner's secret store (`AIKOQL_LICENSE_SK` on
the signing machine) — **never** in the repo, CI, or a release artifact.
Key rotation in v1 = ship a new release with the new public key.

## Configuration (all envs use the `AIKOQL_` prefix)

| env | meaning |
|---|---|
| `AIKOQL_LICENSE` | path to the license file (default `<data dir>/.aikoql-license`) |
| `AIKOQL_OIDC_ISSUER` | IdP issuer URL (required for SSO; discovery document fetched) |
| `AIKOQL_OIDC_CLIENT_ID` | OIDC client id (required) |
| `AIKOQL_OIDC_CLIENT_SECRET` | OIDC client secret (required) |
| `AIKOQL_OIDC_REDIRECT_URI` | OIDC callback URI (required) |
| `AIKOQL_OIDC_ROLES_CLAIM` | IdP claim carrying roles (default `"roles"`) |
| `AIKOQL_AUDIT_RETENTION_DAYS` | audit-log retention in days (default 90) |
| `AIKOQL_ADMIN_PASSWORD` | seeds the **bootstrap admin on first start only** — see below |

`AIKOQL_ADMIN_PASSWORD` is never a standing credential: it seeds the
bootstrap admin's argon2 hash on the first start of a `multi-user` server,
and login afterwards verifies the stored hash. It does not create or
override credentials on later starts.

## SSO

- Both login surfaces redirect to the IdP: the Studio login **and** the MCP
  OAuth `/oauth/authorize` sign-in — one IdP identity covers Studio and MCP
  clients (e.g. claude.ai connectors).
- Authorization-code + PKCE against the issuer's discovery document; the
  `id_token` is verified against the issuer's JWKS.
- Local password login remains **only as break-glass for the bootstrap
  admin** — it is not the general sign-in path when SSO is configured.
- Config-file users keep working untouched in enterprise mode.

## Users and roles

- In enterprise mode the user store (`users.db`, SQLite, in the data dir)
  is authoritative. Users are auto-provisioned on first OIDC login from
  `id_token.sub`; roles come from the mapped claim.
- Startup refuses (exit 2) if `multi-user` is on and no admin can be seeded
  or claimed.
- v1 roles: **`admin`** and **`viewer`** (read-only). Claim mapping: only an
  exact `"admin"` value becomes admin — anything unknown or missing is
  `viewer`, never admin.
- An empty or missing `roles` value defaults to `viewer` (never admin).
- The subject `admin` is reserved for the break-glass bootstrap admin; SSO
  logins presenting it are refused.
- **Live role lookup**: roles resolve from the store on every request (not
  a snapshot at grant time), so a demotion revokes access immediately.
  `viewer` gates both MCP capability checks and REST/admin-API writes.
- Admin API:
  - `GET /api/license` — license status (tier, org, features, expiry);
    any authenticated session
  - admin-only: `GET /api/users` — list users
  - admin-only: `POST /api/users` — pre-provision an OIDC subject with
    roles **before** their first login (no password field — passwords
    exist only for the bootstrap admin)
  - admin-only: `PUT /api/users/{sub}/roles` — set a user's roles

## Audit retention and export

Community keeps its local audit log unchanged. Enterprise adds retention
rotation at startup (`AIKOQL_AUDIT_RETENTION_DAYS`, default 90) and JSONL
export, both gated on the `audit-export` license flag:

```bash
aikoql-mcp audit --export-jsonl PATH [DB]
```

The Studio has an admin-only download button (`GET /api/audit-export`).
The export refuses to overwrite the live audit log itself.

## Operations notes

- **Last-admin lockout**: live role lookup means an admin can demote the
  only remaining admin via `PUT /api/users/{sub}/roles` and lock the
  deployment out of admin access. Recovery: edit the `users` table directly
  in sqlite (the `roles` column is a JSON array), or restart with
  `AIKOQL_ADMIN_PASSWORD` set so the bootstrap seed runs again (it seeds
  whenever no admin exists), or keep a second admin.
- **Single-node**: enterprise multi-user is single-node in v1. Users and
  roles live in a node-local SQLite file; HA (`--replicate-listen`,
  failover) + multi-user is deferred to a later wave and **unsupported** —
  do not run a multi-user licensed server across replicated nodes.
- **Test seams are debug-only**: `AIKOQL_LICENSE_PUBLIC_KEY` (public-key
  override) and `AIKOQL_OIDC_MOCK` / `AIKOQL_OIDC_MOCK_CLAIMS` (mock IdP)
  exist only in debug builds (`cfg(debug_assertions)`); release builds
  cannot see them.
