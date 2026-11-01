"""AG-60: framework adapters, upstream-example shaped.

Each test drives the framework's own documented flow against a LIVE
aikoql server (the mcp_server fixture) and asserts durability: a FRESH
adapter instance on the same server sees the stored history.

- LlamaIndex: SimpleChatEngine.from_defaults(llm=..., memory=...) is the
  docs example; AikoqlMemory stores every ChatMessage and recalls prior
  turns on get().
- OpenAI Agents SDK: Runner.run(agent, input, session=...) is the docs
  example; AikoqlSession feeds the model the conversation and persists
  each turn. The model is an offline stand-in (Model subclass) that
  records what it saw, so the gate can assert the session feed.

Requires: aikoql-mcp binary built (cargo build -p aikoql-mcp).
Run: pytest tests/test_adapters_ag60.py -v
"""
import asyncio
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))
from aikoql import McpClient
from aikoql.adapters.llamaindex import AikoqlMemory
from aikoql.adapters.openai_agents import AikoqlSession

from agents import Agent, Model, ModelResponse, Runner, Usage


def _client(mcp_server) -> McpClient:
    host_port, _, token = mcp_server
    host, _, port = host_port.partition(":")
    c = McpClient(host, int(port), token).connect()
    c.initialize()  # the bearer rides in initialize
    return c


class _EchoModel(Model):
    """Offline stand-in for the OpenAI model in the upstream example.

    Implements the Model surface Runner needs (get_response for the
    non-streaming run) and records every input history so the test can
    assert what the model SAW.
    """

    def __init__(self) -> None:
        self.inputs: list = []

    async def get_response(self, system_instructions=None, input=None, model_settings=None,
                           tools=None, output_schema=None, handoffs=None, tracing=None,
                           *, previous_response_id=None, conversation_id=None, prompt=None):
        from openai.types.responses import ResponseOutputMessage, ResponseOutputText

        self.inputs.append(input)
        text = ""
        for item in reversed(input or []):
            if isinstance(item, dict) and item.get("role") == "user":
                content = item.get("content")
                if isinstance(content, str):
                    text = content
                elif isinstance(content, list):
                    text = "".join(
                        c.get("text", "") for c in content if isinstance(c, dict)
                    )
                break
        return ModelResponse(
            output=[ResponseOutputMessage(
                type="message", id="msg-1", status="completed", role="assistant",
                content=[ResponseOutputText(type="output_text",
                                            text=f"echo: {text}", annotations=[]),
                ],
            )],
            usage=Usage(requests=1, input_tokens=0, output_tokens=0),
            response_id="echo-1",
        )

    async def stream_response(self, *args, **kwargs):
        raise NotImplementedError("the gate test is non-streaming")


def test_llamaindex_memory_backs_the_chat_engine_example(mcp_server):
    from llama_index.core.base.llms.types import ChatMessage
    from llama_index.core.chat_engine import SimpleChatEngine
    from llama_index.core.llms.mock import MockLLM

    subject = "ag60-llamaindex"
    memory = AikoqlMemory(client=_client(mcp_server), subject=subject)
    engine = SimpleChatEngine.from_defaults(llm=MockLLM(max_tokens=16), memory=memory)
    engine.chat("hello from the gate")
    engine.chat("what did I just say?")

    # durability: a fresh memory instance on the same live server restores
    # the whole history (the chat engine itself only calls put + get)
    again = AikoqlMemory(client=_client(mcp_server), subject=subject)
    hist = again.get_all()
    roles = [m.role.value for m in hist]
    assert roles == ["user", "assistant", "user", "assistant"], roles
    assert hist[0].content == "hello from the gate"
    assert isinstance(hist[0], ChatMessage)

    again.reset()
    assert again.get_all() == []


def test_openai_agents_session_feeds_the_runner(mcp_server):
    model = _EchoModel()
    session = AikoqlSession(session_id="ag60-s1", client=_client(mcp_server))
    agent = Agent(name="echo", model=model)

    async def run() -> None:
        await Runner.run(agent, "remember me", session=session)
        await Runner.run(agent, "what did I say?", session=session)

    asyncio.run(run())

    # the second turn's model saw the first turn — the session memory feed
    assert any("remember me" in str(item) for item in model.inputs[-1])
    # durability: a fresh session object on the same server restores the items
    fresh = AikoqlSession(session_id="ag60-s1", client=_client(mcp_server))
    items = asyncio.run(fresh.get_items())
    assert len(items) == 4, items
    assert items[0]["role"] == "user"
    assert items[0]["content"] == "remember me"
    assert items[1]["type"] == "message"
