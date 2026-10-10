# aikoql — Rust SDK

The reference implementation of the canonical Database API for [aikoql](https://github.com/anckursingh/aikoql): embedded mode (`open_engine` + `Kernel::open`) and remote mode over both the MCP transport and the native framed binary protocol.

```rust
use aikoql_sdk::Client;

let mut client = Client::dial("127.0.0.1:9090").await?;
client.with_token("s3cret");
client.initialize().await?;
// native transport (--native-port):
let native = Client::connect_native("127.0.0.1:9091").await?;
```

- Embedded: `aikoql_sdk::Kernel::open("kb")` — the same opener the server uses
- Tests: `AIKOQL_MCP_BIN=$PWD/target/release/aikoql-mcp cargo test -p aikoql-sdk`
- Fuzz (8 libFuzzer targets): `cd fuzz && cargo fuzz run <target>`
- License: Apache-2.0
