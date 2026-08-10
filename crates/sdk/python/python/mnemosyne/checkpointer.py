"""Compatibility re-export for the original LangGraph checkpointer spike.

New code should import from `mnemosyne.adapters.langgraph` directly.
"""

from mnemosyne.adapters.langgraph import MnemosyneLangGraphSaver as MnemosyneCheckpointer

__all__ = ["MnemosyneCheckpointer"]
