# aikoql-mcp

The [aikoql](https://github.com/anckursingh/aikoql) knowledge database shipped as an npm package: `run.js` downloads the platform binary from the matching GitHub Release, verifies its SHA-256, and execs it — `npm@X` always runs native binary `X`.

```bash
npm install -g aikoql-mcp
aikoql-mcp --version
aikoql-mcp serve ./kb              # MCP server over stdio
```

`AIKOQL_BINARY=/path/to/aikoql-mcp` skips the download and runs a local binary (the CI tarball smoke uses this because a PR's version has no Release yet).

- License: Apache-2.0
