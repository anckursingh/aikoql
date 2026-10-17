# aikoql — Python SDK

The first-party Python client for the [aikoql](https://github.com/anckursingh/aikoql) knowledge database: the canonical Database API (remember, get, query, traverse, hybrid search, context compile, prepare/execute) over the MCP transport, plus a connection pool, prepared statements, and a CrewAI memory adapter.

```python
from aikoql import McpClient, Pool

client = McpClient("127.0.0.1", 9090, token="s3cret")
client.connect()
agent = client.agent()
agent.remember("person", {"name": "ada"})
print(agent.query("MATCH person RETURN *"))
```

```bash
pip install aikoql
```

- Install from source: `pip install .` (maturin builds the Rust core; one abi3 wheel covers Python 3.9+)
- Tests (the fuzz estate included): `python -m pytest tests/`
- License: Apache-2.0
