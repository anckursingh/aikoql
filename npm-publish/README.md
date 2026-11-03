# aikoql-mcp

**Persistent, progressive memory for AI agents.** AIKOQL is a knowledge
database your agents write to and read from — every fact stays durable,
versioned, and queryable across sessions, so an agent no longer re-reads
the whole context on every turn. Measured on the aikoql repository: an
agent pulling recall slices instead of re-reading the corpus reads
**99.95% fewer tokens** (see
[benchmarks/results/token-savings.json](https://github.com/anckursingh/aikoql/blob/main/benchmarks/results/token-savings.json)
for the harness and provenance).

The npm package downloads the platform binary from the matching GitHub
Release, verifies its SHA-256, and execs it — `npm@X` always runs native
binary `X`.

```bash
npm install -g aikoql-mcp
aikoql-mcp --version
aikoql-mcp serve ./kb              # MCP server over stdio
```

`AIKOQL_BINARY=/path/to/aikoql-mcp` skips the download and runs a local binary (the CI tarball smoke uses this because a PR's version has no Release yet).

- License: AGPL-3.0-only
