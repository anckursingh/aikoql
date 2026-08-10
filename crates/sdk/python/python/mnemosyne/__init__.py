"""Mnemosyne Python SDK — Agent-first Knowledge Database (MRFC-0040).

Unified interface for AI agents. Supports embedded (PyO3) and server (MCP) modes.

    from mnemosyne import Agent

    # Embedded mode (in-process):
    db = Agent.connect("./kb.redb")

    # Server mode (talks to mnemosyne-mcp over TCP):
    db = Agent.connect("localhost:9090")

    result = db.remember(type_name="Task", properties={"title": "Fix auth bug"})
    tasks = db.aikoql("MATCH Task WHERE status == 'open' RETURN *")
"""

from mnemosyne.agent import Agent
from mnemosyne.mcp_client import McpClient, McpError
from mnemosyne.adapters.crewai import MnemosyneCrewAIMemory
from mnemosyne.adapters.langgraph import MnemosyneLangGraphSaver

# PyO3 native module — may be unavailable in pure-MCP deployments.
try:
    from mnemosyne._mnemosyne import Mnemosyne
except ImportError:
    Mnemosyne = None  # type: ignore

__all__ = [
    "Agent", "McpClient", "McpError",
    "Mnemosyne", "MnemosyneCrewAIMemory", "MnemosyneLangGraphSaver",
]
