"""Framework adapters for LangGraph, CrewAI, LlamaIndex and the OpenAI
Agents SDK."""

from aikoql.adapters.crewai import AikoqlCrewAIMemory
from aikoql.adapters.langgraph import AikoqlLangGraphSaver

__all__ = [
    "AikoqlCrewAIMemory",
    "AikoqlLangGraphSaver",
    "AikoqlMemory",
    "AikoqlSession",
]


def __getattr__(name: str):
    # the AG-60 adapters import their frameworks at module import — lazy
    # here so `import aikoql` works without llama-index / openai-agents
    # installed (the frameworks are only the adapter gate's pins)
    if name == "AikoqlMemory":
        from aikoql.adapters.llamaindex import AikoqlMemory

        return AikoqlMemory
    if name == "AikoqlSession":
        from aikoql.adapters.openai_agents import AikoqlSession

        return AikoqlSession
    raise AttributeError(name)
