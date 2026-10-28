# Test-only TLS fixtures (MRFC-0080 R3c)

Throwaway certificates for the replication TLS tests — **not secrets**,
never used outside the test suite. Generated once with `openssl`
(P-256, valid 100 years); the CA private keys were deleted after signing.

- `ca.pem` — the test CA the standby trusts
- `server.pem` / `server.key` — the leader's certificate (SAN IP:127.0.0.1, DNS:localhost), signed by `ca.pem`
- `other-ca.pem` — an unrelated CA: a standby trusting only it must refuse the leader
