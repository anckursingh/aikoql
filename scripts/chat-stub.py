#!/usr/bin/env python3
"""Deterministic OpenAI-style stub model for the tenant-chat harness.

It is NOT a language model: the query skill always declines (so the server's
retrieval fallback supplies the facts) and the answer skill quotes the first
retrieved statement. It proves the harness, the refusal path and the
grounding check end to end; it says nothing about answer quality.

    python3 scripts/chat-stub.py --port 8099
"""
import argparse
import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer


def reply_for(prompt: str) -> dict:
    if "Knowledge context:" not in prompt:
        return {"query": None, "refusal_reason": "stub model does not write queries"}
    ctx = prompt.split("Knowledge context:\n", 1)[1].split("\n\n", 1)[0]
    facts = [m.group(1) for m in re.finditer(r"^- (.+)$", ctx, re.M)]
    if not facts:
        return {"answer": "UNKNOWN: no context", "grounded": False, "claims": [],
                "refusal_reason": "no context"}
    return {"answer": facts[0], "grounded": True,
            "claims": [{"statement": facts[0], "evidence_ids": ["stub"]}],
            "refusal_reason": None}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        prompt = body["messages"][0]["content"]
        out = json.dumps({"choices": [{"message": {"role": "assistant",
                                                    "content": json.dumps(reply_for(prompt))}}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8099)
    HTTPServer(("127.0.0.1", ap.parse_args().port), Handler).serve_forever()
