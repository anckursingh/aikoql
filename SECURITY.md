# Security Policy

## Reporting

If you find a security issue, **do not open a public issue**. Report it
privately to the maintainer (GitHub: "Report a vulnerability" on the
Security tab, or email the address in the package metadata).

What happens next: the maintainer acknowledges within a few days, fixes in
a private branch, and coordinates disclosure — a patched release first,
then a public advisory with credit to the reporter.

Response time is best-effort; aikoql is community-maintained and there is
no support SLA for the community edition.

## Scope

- The aikoql server (MCP + REST + Studio), the storage engine, encryption
  and key management (KMS), the OAuth endpoints.
- The first-party SDKs (Go, Java, Python, Rust, TypeScript).
- The Studio web UI.

Out of scope: issues in services aikoql is deployed *on* or connects to
(third-party object storage, tunnel providers, cloud IAM) — report those
to their vendors.

## Supported versions

Only the latest release line (**0.4.x**) receives security fixes. Older
versions are unsupported.
