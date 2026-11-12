# Enterprise Tiers Design — aikoql

> Date: 2026-11-03 · Status: owner decisions recorded, ready for review ·
> Owner decisions in **bold**, builder calls marked *(call)*.
> Follows: `docs/superpowers/specs/2026-11-03-community-launch-design.md`.

## Intent & why

One binary, two tiers. Community (free, AGPL-3.0-only) keeps everything shipped
today. Enterprise unlocks a set of features behind an **offline-verified license
key**. This is the "fence" the launch spec already drew: same software inside
customer infra; license + SSO are the boundary, and OIDC/SAML federation was
named the first enterprise-sales blocker (launch spec gap #64). The managed
tier (cloud.aikoql.com) consumes this same enterprise edition.

## Goals

1. Offline license key (ed25519) gates enterprise features. No key → community
   mode, byte-for-byte today's behavior. **Nothing is taken away from community.**
2. OIDC SSO federation: authorization-code + PKCE, issuer discovery. **SSO
   replaces both login surfaces — the Studio login AND the MCP OAuth
   `/oauth/authorize` sign-in** — so one IdP identity covers Studio and MCP
   clients (e.g. claude.ai connectors).
3. Multi-user store with role assignment, wired into the existing
   `authz.rs` role checks — the real "RBAC" claim.
4. Audit retention + export (community keeps its local audit log).
5. Studio + admin API surface: license status, user management.

## Non-goals

- No phone-home, ever. Air-gapped on-prem must work.
- No multi-tenancy inside the engine. Managed = per-customer isolated
  instances + a control plane; that is a **separate design**, not this one.
- No SAML in v1 (OIDC first; SAML is a later wave).
- No separate enterprise binary, no feature-flag compilation split.
- No billing, metering, or quotas.

## Anti-goals

- A keygen path that leaks the private key into the repo or a release
  artifact (license forgery).
- Gating that breaks community: without a license file the server must behave
  exactly as v0.4.0 does today.
- An SSO implementation that can only be tested against a real IdP.
- Expiry + clock skew silently bricking a paying on-prem customer.

## Constraints

- Rust workspace; all code in `crates/services/api/mcp` (+ tests). Same binary.
- AGPL-3.0-only everywhere, per the launch spec.
- Repo discipline: TDD RED-first; commit streams end with the dogfood re-stamp
  of `docs/PR6-TDD-DISPOSITIONS.md`; `docs/superpowers/` stays untracked.
- License verification is fully offline.
- Private key never committed, never in release binaries. Public key embedded
  in the binary at build time.

## Decided — the how

**License key format.** JSON payload + ed25519 signature, base64url-encoded:

```json
{ "ver": 1, "org": "Acme Corp", "issued_at": "...", "expires_at": "...",
  "features": ["oidc-sso", "multi-user", "audit-export"] }
```

- `expires_at` optional (perpetual keys allowed); `features` may contain
  `"all"` as a convenience superset of the three v1 flags.
- *(call)* `oidc-sso` implies `multi-user` (OIDC auto-provisions users);
  `audit-export` covers both retention and export. Keys we issue will carry
  `"all"` or the full set.
- Key file: `.aikoql-license` in the data dir, or path from env
  `AIKOQL_LICENSE`.
- Verification at startup. Invalid/expired key → server **still runs** in
  community mode with a logged warning; enterprise features are disabled.
  Expiry gets a **72h grace** against clock skew.
- One embedded public key in v1; rotation = ship a new release with the new
  key. (A `kid`/multi-key scheme is a later wave if rotation cadence demands.)

**OIDC SSO.** Authorization-code + PKCE against the issuer's discovery
document. Config via env: `AIKOQL_OIDC_ISSUER`, `AIKOQL_OIDC_CLIENT_ID`,
`AIKOQL_OIDC_CLIENT_SECRET`, optional `AIKOQL_OIDC_ROLES_CLAIM` (default
`"roles"`). **SSO covers both login surfaces**: the Studio login redirects to
the IdP, and `/oauth/authorize` redirects to the IdP instead of rendering the
password form. Static `--tcp-token` stays as-is — it is machine auth, not a
user. Local password login remains **only as break-glass for the bootstrap
admin**; it is not the general sign-in path when SSO is configured. `id_token`
is verified against the issuer's JWKS (not decode-only). Users are
auto-provisioned on first login from `id_token.sub`; roles come from the
mapped claim. **Live role lookup**: `OAuth::validate` and Studio session
checks resolve roles from the user store on every request (not a snapshot at
grant time), so claim changes revoke access without waiting for session
expiry. OIDC must sit behind a trait with a mock provider so tests run without
a real IdP (same pattern as `MockEmbeddingProvider`).

**Multi-user store.** SQLite table in the data dir: users (id, sub, name,
roles, created_at). **In enterprise mode this store is authoritative**;
existing config-file users keep working untouched (no migration in v1, and the
community-mode guarantee is unaffected — a server without a valid license is
exactly v0.4.0). A bootstrap admin is seeded on first start from env
`AIKOQL_ADMIN_PASSWORD` when `multi-user` is enabled; startup refuses if
`multi-user` is on and no admin can be seeded or claimed. **No local password
users beyond the bootstrap admin** — admin-API user creation pre-provisions
OIDC subjects (sub + roles) before their first login. v1 roles: **`admin`**
and **`viewer`** (read-only). *(call)* IdP claim values map to `{admin,
viewer}`; unknown or missing claim → viewer. `viewer` gates **both** MCP
capability checks and REST/admin-API writes — today `check_capability` runs
only in the MCP dispatcher, so extending the gate to REST closes a real
authorization hole.

**Audit.** Keep `audit.rs` as-is for community. Enterprise adds retention
(env `AIKOQL_AUDIT_RETENTION_DAYS`, default 90) with rotation, and JSONL
export via an admin command + a Studio download button.

**Studio + admin API.** Login card shows license status (org, features,
expiry) when a key is present. Admin API (auth-guarded): `GET /api/license`,
user list / create / role endpoints.

**Keygen + custody.** **The production ed25519 keypair is generated by the
builder: the public key is committed as a constant in the source (release
builds need no secrets), and the private key is handed to the owner
off-repo** — it never enters the repo, CI, or any release artifact. Owner-side
keygen tool under `scripts/` reads the private key from the owner's env and is
**not** shipped in release binaries. Issuing the first license key is part of
this plan's scope.

## Left to the builder

Exact module layout in `crates/services/api/mcp`, the OIDC trait and mock,
admin REST routes, Studio JS/CSS, error codes, tests (RED-first), and doc
updates. The implementation plan follows this spec after approval.

## Known limitations (v1, deliberate)

- **Enterprise multi-user is single-node.** Users/roles live in a node-local
  SQLite file; HA (`--replicate-listen`, failover) + multi-user is deferred
  to a later wave where auth state rides the replicated store. Documented as
  unsupported, not silently broken.

## Sequencing (owner note)

Design-now / build-after: the open launch leftovers (Windows exe,
Cloudflare R2 verification) stay ahead of this work in the queue unless the
owner reorders.
