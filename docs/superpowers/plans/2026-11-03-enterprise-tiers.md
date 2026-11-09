# Enterprise Tiers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the enterprise edition to the aikoql-mcp binary: an offline-verified ed25519 license key gating OIDC SSO, multi-user RBAC, and audit retention/export — without changing unlicensed behavior.

**Architecture:** One binary, two tiers. A `license.rs` module verifies the key at startup and exposes a `LicenseState`; a SQLite `users.rs` store becomes authoritative in enterprise mode; a hand-rolled `oidc.rs` (ureq + ring RS256 + PKCE, same style as `s3.rs` SigV4) federates login for Studio and the MCP `/oauth/authorize` flow; `authz.rs` gains a `viewer` role and the capability gate extends to REST writes. Community mode is untouched: no license file ⇒ exactly v0.4.0 behavior.

**Tech Stack:** Rust, ring 0.17 (ed25519 sign/verify, RSA-PKCS1 RS256 verify — already in tree via rustls 0.23), rusqlite (already in tree via aikoql-sqlite; added as a direct dep of aikoql-mcp), ureq, sha2, base64, argon2, serde_json.

**Spec:** `docs/superpowers/specs/2026-11-03-enterprise-tiers-design.md` — the plan argues from the spec, so the spec travels with it; executors read both.

## Global Constraints

- One binary (`aikoql-mcp`); all new code in `crates/services/api/mcp` (+ its `tests/`), plus the owner keygen tool in `scripts/license-keygen/`.
- No new Cargo.lock entries. ring and rusqlite are already locked; rusqlite becomes a direct dependency of aikoql-mcp at the locked version. No new async runtime — the binary is blocking (ureq); the only tokio is the existing current-thread one for the KMS SDK bridge.
- Unlicensed server must be byte-for-byte v0.4.0 behavior: config users, TCP tokens, password login, tool gating — all unchanged. Invalid/expired license degrades to community mode with a logged warning, never a crash.
- Env prefix `AIKOQL_` for all new config.
- TDD RED-first for every task; one milestone = one commit set on one branch; the commit stream ends with the dogfood re-stamp of `docs/PR6-TDD-DISPOSITIONS.md` as the tip (`python scripts/dogfood-review-loop.py full` with `AIKOQL_BIN` set, py3.12 venv with `mcp`).
- `docs/superpowers/` stays untracked; never `git add -A`.
- AGPL-3.0-only everywhere; the production license private key never enters the repo, CI, or release artifacts.

- **Attribution** — every commit in this plan's commit steps ends with `Co-Authored-By: Claude Code <noreply@anthropic.com>`.

## Review Focus

The five failure modes most likely to bite a real customer, with the test that pins each (added to the owning task):

1. **Malformed/tampered license at startup** — must degrade to community mode + warning, never crash. Test in Task 2: `tampered_key_runs_as_community`.
2. **Expiry boundary** — a key expired ≤72h ago still works (grace); >72h disables enterprise features. Test in Task 2: `expiry_grace_window`.
3. **`viewer` bypassing REST** — capability checks historically ran only in the MCP dispatcher; a `viewer` must get 403 on REST/admin writes. Test in Task 6: `viewer_cannot_post_rest_write`.
4. **OIDC claim escalation** — unknown or missing role claim must yield `viewer`, never `admin`. Test in Task 5: `unknown_claim_maps_to_viewer`.
5. **Unlicensed parity** — with no license, TCP tokens, config users and password login behave exactly as v0.4.0 (no enterprise code path executes). Test in Task 3: `unlicensed_server_unchanged`.

---

### Task 1: Owner keygen tool + production keypair

**Files:**
- Create: `scripts/license-keygen/Cargo.toml` (standalone crate — **not** a workspace member, `publish = false`), `scripts/license-keygen/src/main.rs`
- Test: in-module `#[cfg(test)]` in `main.rs`

**Interfaces:**
- Produces: the production ed25519 keypair (private key → owner, off-repo; public key → committed in Task 2). CLI: `keygen` (prints new keypair, base64 lines, prefixed `SK=`/`PK=`), `sign --org ORG --expires YYYY-MM-DD --features all` (reads the private key from env `AIKOQL_LICENSE_SK`, prints the license blob in Task 2's envelope format). Per spec: owner-side tool under `scripts/`, key from the owner's env, never shipped in release binaries.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn license_roundtrip_sign_then_verify() {
    let (sk, pk) = generate_pair();
    let blob = sign_license(&sk, &LicensePayload { ver: 1, org: "test".into(), issued_at: 0, expires_at: None, features: vec!["all".into()] });
    assert!(verify_license_blob(&pk, &blob).is_ok());
    let mut tampered = blob.clone(); tampered[tampered.len() - 1] ^= 1;
    assert!(verify_license_blob(&pk, &tampered).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path scripts/license-keygen/Cargo.toml`
Expected: FAIL (compile error — `generate_pair`, `sign_license`, `verify_license_blob` not defined)

- [ ] **Step 3: Implement keygen + sign in `scripts/license-keygen/src/main.rs`**

ring `Ed25519KeyPair`; envelope = `base64url(json_payload) + "." + base64url(signature)` — exactly the format Task 2's verifier parses. `--features` accepts `all` or a comma list of `oidc-sso,multi-user,audit-export`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path scripts/license-keygen/Cargo.toml`
Expected: PASS

- [ ] **Step 5: Generate the production pair (owner custody)**

Run: `cargo run --manifest-path scripts/license-keygen/Cargo.toml -- keygen > ~/.aikoql-license-keypair.txt`
Expected: file contains `SK=` and `PK=` lines. **The private key must not be pasted into chat** — tell the owner the file path; the owner moves and stores it. Only the public key is used in Task 2.

- [ ] **Step 6: Commit**

```bash
git add scripts/license-keygen/
git commit -m "feat(enterprise): owner license-keygen tool (ed25519 sign/verify)"
```

### Task 2: License module + startup wiring

**Files:**
- Create: `crates/services/api/mcp/src/license.rs`
- Modify: `crates/services/api/mcp/src/main.rs` (declare module + startup verification), `src/config.rs` (`AIKOQL_LICENSE` path or `<data-dir>/.aikoql-license`)
- Test: `crates/services/api/mcp/tests/license.rs`

**Interfaces:**
- Consumes: production public key (Task 1, Step 5).
- Produces: `pub(crate) struct LicenseState { pub org: String, pub expires: Option<SystemTime>, pub features: FeatureSet }`; `pub(crate) struct FeatureSet { pub oidc_sso: bool, pub multi_user: bool, pub audit_export: bool }` with `FeatureSet::from_flags(&[String])` where `oidc_sso ⇒ multi_user`; `pub(crate) fn verify_license(bytes: &[u8]) -> Result<LicenseState, LicenseError>` with a 72h expiry grace; `pub(crate) enum LicenseError`; `pub(crate) const LICENSE_PUBLIC_KEY_B64: &str`. Consumed by Tasks 5, 7, 8, 9.

- [ ] **Step 1: Write the failing tests** (fixtures signed with a TEST keypair — a test-only helper in the test file using ring, never the production key)

```rust
#[test] fn valid_key_parses() { /* fixture → LicenseState { org: "test", features: all } */ }
#[test] fn tampered_key_rejected() { /* flip last byte → Err */ }
#[test] fn malformed_base64_rejected() { /* "not-a-license" → Err */ }
#[test] fn expired_beyond_grace_rejected() { /* expires 73h ago → Err */ }
#[test] fn expiry_grace_window() { /* expires 71h ago → Ok */ }
#[test] fn oidc_sso_implies_multi_user() { /* flags ["oidc-sso"] → both true */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test license`
Expected: FAIL (module/function not defined)

- [ ] **Step 3: Implement `license.rs`**

ring `UnparsedPublicKey<Ed25519>` verification against `LICENSE_PUBLIC_KEY_B64`; parse `ver`, `org`, `issued_at`, `expires_at`, `features`; `SystemTime` grace = 72h.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test license`
Expected: PASS (all 6)

- [ ] **Step 5: Wire startup + degradation**

In `main.rs`/`config.rs`: read `AIKOQL_LICENSE` (path) else `<data-dir>/.aikoql-license`; `verify_license` → set the global `LicenseState`; on `Err` or missing file → community mode + `tracing::warn!("license rejected: {err}; running community mode")`. Add integration test:

```rust
#[test]
fn tampered_key_runs_as_community() { /* spawn server with AIKOQL_LICENSE=fake → starts, health endpoint responds, log contains "community mode" */ }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test license`
Expected: PASS (all)

- [ ] **Step 7: Commit**

```bash
git add crates/services/api/mcp/src/license.rs crates/services/api/mcp/src/main.rs crates/services/api/mcp/src/config.rs crates/services/api/mcp/tests/license.rs
git commit -m "feat(enterprise): offline ed25519 license verification with community-mode degradation"
```

### Task 3: Multi-user store

**Files:**
- Create: `crates/services/api/mcp/src/users.rs`
- Modify: `crates/services/api/mcp/Cargo.toml` (`rusqlite = "<locked version>"` — already in Cargo.lock), `src/main.rs`, `src/config.rs`
- Test: `crates/services/api/mcp/tests/users.rs`

**Interfaces:**
- Produces: `pub(crate) struct UserStore { conn: rusqlite::Connection }` with `open(path: &Path) -> Result<Self>`, `seed_bootstrap_admin(password: &str) -> Result<bool>` (argon2 hash of the password, same crate/params the kernel KMS uses; returns false if an admin already exists), `has_admin() -> bool`, `upsert_oidc_user(sub: &str, name: Option<&str>, roles: &[String]) -> Result<()>`, `set_roles(sub: &str, roles: &[String]) -> Result<()>`, `roles_of(sub: &str) -> Result<Option<Vec<String>>>`, `list_users() -> Result<Vec<UserRow>>`. Schema: `users(id INTEGER PRIMARY KEY, sub TEXT NOT NULL UNIQUE, name TEXT, roles TEXT NOT NULL, created_at TEXT NOT NULL)`. Consumed by Tasks 5, 6, 8.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn bootstrap_admin_seeded_once() { /* second seed returns false, one row */ }
#[test] fn oidc_upsert_and_roles() { /* upsert creates; roles stored */ }
#[test] fn set_roles_overwrites() { /* second write replaces, no dup row */ }
#[test] fn roles_of_unknown_sub_none() { /* absent sub → None */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test users`
Expected: FAIL (module not defined)

- [ ] **Step 3: Implement `users.rs`**

rusqlite; roles serialized as a JSON array in `roles TEXT`; `created_at` RFC 3339 UTC.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test users`
Expected: PASS (all 4)

- [ ] **Step 5: Wire startup refuse + parity**

When `multi_user` is enabled (license or env): open the store in the data dir; if `!has_admin()` and `AIKOQL_ADMIN_PASSWORD` is unset → startup error `multi-user requires a bootstrap admin (set AIKOQL_ADMIN_PASSWORD)`. Integration tests:

```rust
#[test] fn multi_user_without_admin_refuses_start() { /* licensed + no admin, no env password → non-zero exit, error message */ }
#[test] fn unlicensed_server_unchanged() { /* no license → no users.db created; tcp-token auth and password login behave as v0.4.0 */ }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test users`
Expected: PASS (all)

- [ ] **Step 7: Commit**

```bash
git add crates/services/api/mcp/src/users.rs crates/services/api/mcp/src/main.rs crates/services/api/mcp/src/config.rs crates/services/api/mcp/Cargo.toml crates/services/api/mcp/tests/users.rs
git commit -m "feat(enterprise): sqlite user store with bootstrap admin and startup refuse"
```

### Task 4: OIDC client (hand-rolled, like `s3.rs` SigV4)

**Files:**
- Create: `crates/services/api/mcp/src/oidc.rs`
- Create: `crates/services/api/mcp/tests/fixtures/oidc-test-rsa.pem` — test-only RSA keypair (openssl `genrsa`), committed, marked test-only in a README line; never a production key
- Test: in-module `#[cfg(test)]` in `oidc.rs`

**Interfaces:**
- Produces: `pub(crate) trait OidcProvider { fn authorize_url(&self, state: &str, verifier: &str) -> Result<String, OidcError>; fn exchange(&self, code: &str, verifier: &str) -> Result<IdTokenClaims, OidcError>; }`; `pub(crate) struct HttpOidcProvider` (ureq; discovery from `AIKOQL_OIDC_ISSUER/.well-known/openid-configuration`; JWKS fetch; RS256 verify via ring `RSA_PKCS1_2048_8192_SHA256`; PKCE S256 via sha2 + getrandom); `pub(crate) struct MockOidcProvider` (signs tokens with the fixture key); `pub(crate) struct IdTokenClaims { pub sub: String, pub name: Option<String>, pub roles: Vec<String> }` (roles from `AIKOQL_OIDC_ROLES_CLAIM`, default `"roles"`); `pub(crate) enum OidcError`. Consumed by Task 5.

- [ ] **Step 1: Write the failing tests** (against `MockOidcProvider`)

```rust
#[test] fn exchange_valid_token_parses_claims() { /* mock token → sub/name/roles */ }
#[test] fn rs256_bad_signature_rejected() { /* tampered token → OidcError */ }
#[test] fn roles_claim_missing_yields_empty() { /* no "roles" in claims → empty vec (caller maps to viewer) */ }
#[test] fn pkce_verifier_matches_s256() { /* verifier ↔ code_challenge round-trip */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp oidc`
Expected: FAIL (module not defined)

- [ ] **Step 3: Implement `oidc.rs`**

Blocking ureq calls (the binary has no general async runtime); id_token verification = header/payload parse + signature check against the JWKS keys; `exp`/`aud`/`iss` checked.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp oidc`
Expected: PASS (all 4)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/oidc.rs crates/services/api/mcp/tests/fixtures/oidc-test-rsa.pem
git commit -m "feat(enterprise): hand-rolled OIDC client (discovery, JWKS RS256, PKCE) with mock provider"
```

### Task 5: OIDC wire-in (Studio login + MCP `/oauth/authorize`)

**Files:**
- Modify: `crates/services/api/mcp/src/oauth.rs` (authorize redirect + live role lookup in `OAuth::validate`), `src/http.rs` (callback route + break-glass), `src/studio.rs` (login surface), `src/session.rs`
- Test: `crates/services/api/mcp/tests/oidc_sso.rs`

**Interfaces:**
- Consumes: `LicenseState` (T2), `UserStore` (T3), `OidcProvider` (T4).
- Produces: enterprise login flow. With `oidc_sso` enabled and `AIKOQL_OIDC_ISSUER` set: Studio login shows "Sign in with SSO" (redirect → IdP → callback exchanges code → `upsert_oidc_user` → session); `/oauth/authorize` redirects to the IdP instead of rendering the password form; `OAuth::validate` and Studio session checks resolve roles live via `UserStore::roles_of(sub)` (config-file users keep the existing `AuthResolver` path); bootstrap admin password login always works as break-glass. Unknown/missing claim → `viewer`, never `admin`.

- [ ] **Step 1: Write the failing tests** (mock provider wired through test-only config)

```rust
#[test] fn sso_login_creates_session_and_user() { /* full redirect→callback; session cookie set; user row exists */ }
#[test] fn oauth_authorize_redirects_to_idp() { /* GET /oauth/authorize → 302 to IdP authorize_url */ }
#[test] fn unknown_claim_maps_to_viewer() { /* claim "janitor" → roles == ["viewer"] */ }
#[test] fn breakglass_admin_login_works_when_idp_configured() { /* password login with bootstrap admin still 200 */ }
#[test] fn live_role_revocation() { /* flip store roles after grant → next MCP call denied */ }
#[test] fn unlicensed_studio_password_login_unchanged() { /* no license → v0.4.0 password path, no SSO button */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test oidc_sso`
Expected: FAIL (missing behavior)

- [ ] **Step 3: Implement the flow**

`OidcProvider` selected by config (`HttpOidcProvider` unless test mock); callback route `/oidc/callback` exchanges and starts the existing session model; authorize redirect only when licensed + configured, else the current password form (byte-for-byte).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test oidc_sso`
Expected: PASS (all 6)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/oauth.rs crates/services/api/mcp/src/http.rs crates/services/api/mcp/src/studio.rs crates/services/api/mcp/src/session.rs crates/services/api/mcp/tests/oidc_sso.rs
git commit -m "feat(enterprise): OIDC SSO for Studio and MCP authorize with live role lookup"
```

### Task 6: RBAC `viewer` + REST gate

**Files:**
- Modify: `crates/services/api/mcp/src/authz.rs`, `src/api_rest.rs`, `src/http.rs`
- Test: `crates/services/api/mcp/tests/rbac_rest.rs`

**Interfaces:**
- Consumes: `UserStore` (T3).
- Produces: `viewer` role in `check_capability` (read-only tools allowed, writes blocked, same fail-closed shape as the existing restricted map); `pub(crate) fn check_rest(roles: &[String], admin_only: bool) -> Result<(), ApiError>` applied to REST write routes (POST/PUT/DELETE: `admin_only` for admin routes, `admin || !viewer` for writes) — closing the hole where REST bypassed capability checks; `admin` passes everything, unchanged.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn viewer_can_call_read_tools_over_mcp() { /* remember/recall/search read path OK */ }
#[test] fn viewer_blocked_on_write_tools() { /* epistemic-state-changing tool → -32001 */ }
#[test] fn viewer_cannot_post_rest_write() { /* viewer session POSTs a write route from http.rs (use the actual route name) → 403 */ }
#[test] fn admin_passes_all_gates() { /* admin over REST write + MCP */ }
#[test] fn unlicensed_tcp_token_roles_unchanged() { /* existing "admin"/read-scope token behavior identical */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test rbac_rest`
Expected: FAIL (REST writes currently succeed for any session — the hole)

- [ ] **Step 3: Implement the gate**

Extend `authz.rs` restricted map with `viewer`; route REST writes through `check_rest` using the session's live roles.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test rbac_rest`
Expected: PASS (all 5)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/authz.rs crates/services/api/mcp/src/api_rest.rs crates/services/api/mcp/src/http.rs crates/services/api/mcp/tests/rbac_rest.rs
git commit -m "feat(enterprise): viewer role gates MCP tools and REST writes (closes REST bypass)"
```

### Task 7: Audit retention + export

**Files:**
- Modify: `crates/services/api/mcp/src/audit.rs`, `src/admin.rs`, `src/config.rs`
- Test: `crates/services/api/mcp/tests/audit_export.rs`

**Interfaces:**
- Consumes: `LicenseState` (T2).
- Produces: retention via `AIKOQL_AUDIT_RETENTION_DAYS` (default 90) — startup rotation deletes entries older than the cutoff; `run_audit` gains `--export-jsonl PATH` (requires license flag `audit_export`; without it: error `enterprise feature: audit export`); community's local audit log untouched.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn retention_rotates_old_entries() { /* seed old + fresh entries, restart with retention=1 → old gone */ }
#[test] fn default_retention_is_90_days() { /* no env → 90-day cutoff */ }
#[test] fn export_writes_jsonl_lines() { /* --export-jsonl → one JSON object per line, parseable */ }
#[test] fn export_requires_enterprise_license() { /* unlicensed → command errors, no file */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test audit_export`
Expected: FAIL (no retention/export behavior)

- [ ] **Step 3: Implement retention + export**

Cutoff computed from RFC 3339 timestamps already in the log; export = streaming JSONL.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test audit_export`
Expected: PASS (all 4)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/audit.rs crates/services/api/mcp/src/admin.rs crates/services/api/mcp/src/config.rs crates/services/api/mcp/tests/audit_export.rs
git commit -m "feat(enterprise): audit retention rotation and gated JSONL export"
```

### Task 8: Admin API

**Files:**
- Modify: `crates/services/api/mcp/src/http.rs`, `src/api_rest.rs`
- Test: `crates/services/api/mcp/tests/admin_api.rs`

**Interfaces:**
- Consumes: `LicenseState` (T2), `UserStore` (T3), `check_rest` (T6).
- Produces: `GET /api/license` (any authenticated session; `{tier: "enterprise"|"community", org, features, expires}`), `GET /api/users` (admin only), `POST /api/users` (admin only; `{sub, name?, roles[]}` — pre-provisions OIDC subjects, **no password field**), `PUT /api/users/{sub}/roles` (admin only).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn license_endpoint_reports_enterprise() { /* licensed server + session → tier=enterprise */ }
#[test] fn license_endpoint_says_community_when_unlicensed() { /* tier=community */ }
#[test] fn users_list_requires_admin() { /* viewer → 403; admin → 200 */ }
#[test] fn viewer_cannot_create_user() { /* POST /api/users with viewer session → 403 */ }
#[test] fn create_user_preprovisions_oidc_sub() { /* row exists, can log in later via SSO */ }
#[test] fn set_roles_updates_store() { /* PUT → roles_of reflects change */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp --test admin_api`
Expected: FAIL (routes missing)

- [ ] **Step 3: Implement the routes**

Reuse the existing REST routing/error conventions in `http.rs`/`api_rest.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp --test admin_api`
Expected: PASS (all 6)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/http.rs crates/services/api/mcp/src/api_rest.rs crates/services/api/mcp/tests/admin_api.rs
git commit -m "feat(enterprise): admin API for license status and user management"
```

### Task 9: Studio UI

**Files:**
- Modify: `crates/services/api/mcp/src/studio.rs` (embedded HTML/JS)
- Test: in-module (assert the served login/admin HTML contains the markers)

**Interfaces:**
- Consumes: admin API (T8).
- Produces: login card license status line (org + expiry, or "Community" when unlicensed), "Sign in with SSO" button when configured, admin-only users panel (list, pre-provision, set roles), audit export download button when the license has `audit_export`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn login_card_shows_license_status_marker() { /* served HTML contains data-license-status */ }
#[test] fn login_card_has_sso_button_marker() { /* licensed+configured → data-sso-button present; unlicensed → absent */ }
#[test] fn users_panel_marker_present() { /* admin session HTML contains the users panel anchor */ }
#[test] fn audit_export_button_present_when_licensed() { /* license has audit-export → served HTML has the export button marker; unlicensed → absent */ }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aikoql-mcp studio`
Expected: FAIL (markers missing)

- [ ] **Step 3: Implement the Studio changes**

Follow the existing embedded-HTML structure and CSS variables in `studio.rs`; wire the panel to the Task 8 endpoints.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aikoql-mcp studio`
Expected: PASS (all 4)

- [ ] **Step 5: Commit**

```bash
git add crates/services/api/mcp/src/studio.rs
git commit -m "feat(enterprise): Studio license status, SSO button and admin users panel"
```

### Task 10: Docs + dogfood re-stamp (the tip)

**Files:**
- Modify: the repo docs index/README: enterprise tier section — license file location + envs (`AIKOQL_LICENSE`, `AIKOQL_OIDC_*`, `AIKOQL_AUDIT_RETENTION_DAYS`), roles (`admin`, `viewer`), audit export, the single-node v1 limitation.
- Modify: `docs/PR6-TDD-DISPOSITIONS.md` via the dogfood re-stamp script.

- [ ] **Step 1: Write the docs section**

Enterprise tier usage doc: what a license key is, how the owner issues one (`scripts/license-keygen`), what each flag enables, break-glass admin, and the known limitation (enterprise multi-user is single-node; HA + multi-user deferred).

- [ ] **Step 2: Full local suites**

Run: `cargo test && pytest && node --test` (the local gate — GitHub Actions has no credits)
Expected: all green, zero warnings

- [ ] **Step 3: Dogfood re-stamp**

Run: `AIKOQL_BIN=<path> python scripts/dogfood-review-loop.py full` (py3.12 venv with `mcp`)
Expected: re-stamped `docs/PR6-TDD-DISPOSITIONS.md` diff

- [ ] **Step 4: Commit the tip**

```bash
git add docs/ scripts-affected-by-dogfood
git commit -m "docs: dogfood re-stamp at the enterprise-tiers tip"
```

## Owner actions after the stream

1. Move the production private key (Task 1, Step 5) to your secret store; delete `~/.aikoql-license-keypair.txt`.
2. Issue the first real license (`scripts/license-keygen sign ...`), boot a server with `AIKOQL_LICENSE`, confirm the startup log shows enterprise mode — the end-to-end proof of the chain.
3. The branch is pushed `--no-track`; you merge the PR (repo discipline).

## Sequencing note

Per the spec: design-now / build-after. The open launch leftovers (Windows exe, Cloudflare R2 verification) stay ahead of this plan in the queue unless you reorder.

