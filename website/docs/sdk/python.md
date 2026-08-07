---
title: Python SDK
description: PyO3 native bindings for Mnemosyne
---

# Python SDK

Native Python bindings via PyO3. Direct access to the Knowledge Kernel.

## Installation

```bash
pip install mnemosyne
```

## Usage

```python
import mnemosyne_py

# Open a database
kernel = mnemosyne_py.Kernel.open("./kb.redb")

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

# AIKOQL
result = kernel.aikoql("MATCH Employee RETURN name, role")
```

## LangGraph + CrewAI

Built-in adapters for AI agent frameworks:

```python
from mnemosyne_py.adapters import LangGraphCheckpointer

checkpointer = LangGraphCheckpointer(kernel)
# Use as LangGraph's checkpointer for agent state persistence
```
