---
title: Python SDK
description: PyO3 native bindings for aikoql
---

# Python SDK

Native Python bindings via PyO3. Direct access to the Knowledge Kernel.

## Installation

```bash
pip install aikoql
```

## Usage

```python
import aikoql_py

# Open a database
kernel = aikoql_py.Kernel.open("./kb.redb")

# Create an object
result = kernel.remember({
    "type_name": "Employee",
    "properties": {"name": "Alice", "role": "Architect"},
    "tenant": "acme"
})
print(f"Created: {result.koid}")

# Query
results = kernel.find_similar({
    "type_name": "Employee",
    "text": "engineer"
})
for r in results:
    print(f"{r.koid}: {r.score}")

# aikoql
result = kernel.aikoql("MATCH Employee RETURN name, role")
```

## LangGraph + CrewAI

Built-in adapters for AI agent frameworks:

```python
from aikoql_py.adapters import LangGraphCheckpointer

checkpointer = LangGraphCheckpointer(kernel)
# Use as LangGraph's checkpointer for agent state persistence
```
