#!/usr/bin/env python3
"""Regenerate the chat prompt fixtures from the Python source of truth.

PYTHONPATH=training/src python scripts/gen-chat-prompt-fixtures.py
"""
import json
import pathlib

from aikoql_training.inference import build_answer_prompt, build_query_prompt

QUESTIONS = ["Who holds card 4111?", "What is 1+1?", "Wer hält die Karte? ü中\U0001f600",
             'Quote "this" and {braces}']
STATEMENTS = [[], ["Alice Chen holds card 4111"], ["a", "b é", "c\nd"], ["only one"]]

out = {
    "query": [{"question": q, "prompt": build_query_prompt(q)} for q in QUESTIONS],
    "answer": [{"question": q, "statements": s, "prompt": build_answer_prompt(q, s)}
               for q, s in zip(QUESTIONS, STATEMENTS)],
}
dest = pathlib.Path(__file__).resolve().parent.parent / "crates/services/api/mcp/tests/fixtures/chat/prompts.json"
dest.write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
