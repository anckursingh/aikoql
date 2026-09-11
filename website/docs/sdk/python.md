---
title: Python SDK
description: PyO3 native bindings + MCP client for aikoql
---

# Python SDK

The first-party SDK (adopted P3-M9): PyO3 native bindings for embedded mode
plus a pure-Python MCP client for server mode, unified behind `Agent.connect`.

## Installation

```bash
pip install aikoql
```

## Usage

```python
from aikoql import Agent

# Embedded mode — a fresh path creates an aikoql-v2 database directory
# (existing .redb files open as redb, a v1 WAL opens as v1 — auto-detection,
# never reinterpretation):
db = Agent.connect("./kb")

# Server mode — MCP over TCP (P3-M1 servers require a --tcp-token):
db = Agent.connect("localhost:9090", token="your-tcp-token")

# Create an object
result = db.remember("Employee", {"name": "Alice", "role": "Architect"})
print(f"Created: {result['koid']}")

# Hybrid search
results = db.find_similar(text="engineer", type_name="Employee", k=5)

# aikoql
rows = db.aikoql("MATCH Employee RETURN *")
```

## LangGraph + CrewAI

Built-in adapters for AI agent frameworks:

```python
from aikoql.adapters.langgraph import AikoqlLangGraphSaver
from aikoql.adapters.crewai import AikoqlCrewAIMemory

checkpointer = AikoqlLangGraphSaver(db)
memory = AikoqlCrewAIMemory(db)
```

## Version parity (sdk001)

The package version is the workspace version — `pyproject.toml` is
`dynamic = ["version"]` (maturin reads the crate's `version.workspace`) and
the module exports it:

```python
import aikoql
aikoql.__version__  # == the Cargo workspace version, pinned by the sdk001 test
```

CI runs the full SDK suite (contract tests against the real aikoql-mcp
binary) on every push; releases publish to PyPI via trusted publishing.
