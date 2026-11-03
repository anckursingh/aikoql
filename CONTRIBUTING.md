# Contributing to aikoql

## Local suites (the gate)

GitHub Actions is not currently running on PRs — the local suites are the gate:

```bash
cargo test --workspace --no-fail-fast   # Rust (cargo on PATH: ~/.cargo/bin)
pytest                                   # Python (benchmarks/scripts)
node --test                              # TypeScript SDK
```

The lint gate is `cargo clippy --workspace --all-targets -- -D warnings` —
zero warnings.

## Development discipline

- **TDD, RED first.** Every behavior change starts with a failing test.
- **Read AGENTS.md** before changing anything — commit-stream and milestone
  rules live there.
- **Small commit sets.** One milestone = one commit set.

## DCO

All commits must carry a Developer Certificate of Origin sign-off:

```
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it for you. By signing off you certify (DCO 1.1) that
you wrote the patch or have the right to submit it under the project's
license (AGPL-3.0-only).

## Review flow

- Open a PR from a named branch; the maintainer reviews and merges.
- The maintainer pushes releases and publishes registries — contributors
  never need to.
