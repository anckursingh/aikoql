# checksum8 threat model (PR6-010)

The v2 storage formats fingerprint files and headers with `checksum8` — the
first 8 bytes of SHA-256.

## Guarantee

**Accidental-corruption detection.** Each fingerprint catches bit rot, torn
writes, truncation, and mid-copy changes, with a false-pass probability of
2^-64 per protected region. That is the threat the storage layer actually
faces: files on disk, half-written frames after a crash, copies interrupted
mid-stream.

## Not guaranteed

- **Cryptographic authenticity** — a fingerprint is not a signature.
- **Malicious modification resistance** — anyone who can write the file can
  recompute its fingerprint.
- **Publisher authenticity** — nothing binds a fingerprint to an identity.

## Where checksum8 is used

- WAL frames and the torn-safe prefix (wal.rs)
- segment block headers and segment footers (segment.rs)
- checkpoint files (checkpoint.rs)
- the snapshot marker's per-file list (snapshot.rs)
- manifest and CURRENT structures (format.rs)

Every use answers the same question — "did these bytes change by accident?"
— and only that question.

## The security boundary lives elsewhere

Where an adversary is in scope, the system uses mechanisms built for that
threat, at the layers that own it:

- MRFC-0020 encryption key management (KEK/DEK, authenticated encryption)
- P3-M1 API auth: argon2id password hashing, CSPRNG session tokens,
  token-scoped roles
- transport: TLS and token-authenticated TCP

Storage fingerprints are not that boundary and must not be loaded with that
duty.

## Change rule

Do not blanket-replace checksum8. Per format, ask what it protects against:

- threat = accidental damage → keep checksum8 (8 bytes per protected
  region; the 2^-64 bound is the whole argument)
- threat = tamper evidence → add a keyed MAC (HMAC-SHA256 under the
  MRFC-0020 KEK) to THAT format's design — a longer unkeyed hash still
  provides zero tamper resistance.
