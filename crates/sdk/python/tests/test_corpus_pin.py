"""The §16 pin: the cross-language golden corpus spec exists at
sdk-fuzz-corpus/corpus.json and this SDK's column holds for every case.

Removing a case id is a detected coverage loss; a column mismatch is a
wire-behavior drift (or an undocumented divergence — document it in the
spec's note and re-stamp). Where a real primitive exists the pin calls it
(_parse_version, McpError.from_response, _stream_notify); the request
verdicts are restated inline at the exact source lines, because the real
code reads sockets (_recv_response).
"""

import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
_MODULE = ROOT / "crates" / "sdk" / "python" / "python" / "aikoql" / "mcp_client.py"

# `import aikoql` resolves to the INSTALLED site-packages snapshot (the
# package __init__ pulls the compiled Rust extension), which would pin the
# installed snapshot instead of this repo's source — a repo drift would
# never redden the pin. Load the source file directly instead: it is
# stdlib-only (json/socket/time/uuid/typing), so the file import is viable.
_spec = importlib.util.spec_from_file_location("aikoql_mcp_client_under_test", _MODULE)
_mcp = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _mcp
_spec.loader.exec_module(_mcp)
McpError = _mcp.McpError
_parse_version = _mcp._parse_version
_stream_notify = _mcp._stream_notify

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
        # The real primitive: _stream_notify (mcp_client.py) — the same
        # verdict the aikoql_stream loop runs; python yields the raw params
        # dict as-is.
        p = _stream_notify(frame, "s1")
        if p is None:
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
