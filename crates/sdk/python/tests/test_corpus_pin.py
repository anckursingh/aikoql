"""The §16 pin: the cross-language golden corpus spec exists at
sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.

Removing a case id is a detected coverage loss; a column mismatch is a
wire-behavior drift (or an undocumented divergence — document it in the
spec's note and re-stamp). Where a real primitive exists the pin calls it
(_parse_version, McpError.from_response); the request/stream verdicts are
restated inline at the exact source lines, because the real code reads
sockets (_recv_response, aikoql_stream).
"""

import json
from pathlib import Path

from aikoql.mcp_client import McpError, _parse_version

ROOT = Path(__file__).resolve().parents[4]
CORPUS = ROOT / "sdk-fuzz-corpus" / "corpus.json"

CASE_IDS = [
    "corp-v01", "corp-v02", "corp-v03", "corp-v04", "corp-v05",
    "corp-e01", "corp-e02", "corp-e03", "corp-e04", "corp-e05",
    "corp-r01", "corp-r02", "corp-n01",
    "corp-s01", "corp-s02", "corp-s03",
    "corp-c01", "corp-c02", "corp-c03", "corp-c04",
]
SURFACES = ["version", "rpc_error", "response_id", "nonfinite", "notify", "correlation"]


def _load():
    if not CORPUS.is_file():
        raise AssertionError(
            "the §16 corpus is absent "
            f"({CORPUS.relative_to(ROOT)}): the D-16 golden-corpus slice is missing"
        )
    return json.loads(CORPUS.read_text(encoding="utf-8"))


def _classify(surface, inp):
    if surface == "version":
        return list(_parse_version(inp))
    if surface == "rpc_error":
        frame = json.loads(inp)
        err = frame.get("error")
        # McpError.from_response (mcp_client.py) — the real error renderer.
        e = McpError.from_response(err)
        return {"code": e.code, "message": e.message}
    if surface in ("response_id", "nonfinite"):
        try:
            frame = json.loads(inp)
            rid = frame.get("id")
        except AttributeError:
            # corp-n01: the bare 1e999 token parses to float inf; the real
            # _recv_response's frame.get raises and the error escapes the
            # loop — classification is undefined.
            return "unclassified"
        # _recv_response's verdict, restated (mcp_client.py ~:240-258, want=1).
        if rid is None:
            return "skip"
        if not isinstance(rid, int):
            return "protocol"  # "response id ... is not an integer"
        if rid < 1:
            return "skip"
        if rid > 1:
            return "protocol"
        return "match"
    if surface == "notify":
        frame = json.loads(inp)
        # aikoql_stream's verdict, restated (mcp_client.py ~:488-498,
        # stream_id="s1"): python yields the raw params dict as-is.
        if frame.get("method") != "notifications/notify":
            return {"verdict": "skip", "pair": None}
        p = frame.get("params", {})
        if p.get("stream_id") != "s1":
            return {"verdict": "skip", "pair": None}
        return {"verdict": "yield", "pair": p}
    if surface == "correlation":
        want, got = inp["want"], inp["got"]
        # The correlation contract (aikoql.py): skip on smaller, protocol on
        # larger, match on equal.
        if got < want:
            return "skip"
        if got > want:
            return "protocol"
        return "match"
    raise AssertionError(f"unknown surface {surface}")


def test_corpus_pin():
    spec = _load()
    cases = spec["cases"]
    by_id = {c["id"]: c for c in cases}
    for cid in CASE_IDS:
        assert cid in by_id, f"case {cid} is gone from the §16 corpus — a removed case is a coverage loss"
    have_surfaces = {c["surface"] for c in cases}
    for s in SURFACES:
        assert s in have_surfaces, f"surface {s} has no cases in the §16 corpus"
    for c in cases:
        assert "python" in c["expected"], f"case {c['id']} has no python column in the §16 corpus"
        got = _classify(c["surface"], c["input"])
        want = c["expected"]["python"]
        assert got == want, (
            f"case {c['id']} ({c['surface']}): python column drift — "
            f"got {got!r}, want {want!r}"
        )
