#!/usr/bin/env python3
# PR6-F8 (P1-8): the review loop dogfoods MRFC-0070 (docs/ARCHITECT-
# REVIEW-2026-09.md). Each review finding lives as a Requirement KO in the
# project knowledge base (./kb, served by the repo's own aikoql-mcp
# plugin); every commit that re-stamped the dispositions doc is reconciled
# against the compiled knowledge document via the A8 reconcile tool; and
# the dispositions doc carries a compiled section emitted from kernel
# state, so trace_requirement — not prose — answers "which tests pin
# R3-003". ci.yml's dag job pins the compiled-head stamp: a commit that
# moves the doc without re-emitting fails CI.
#
#   python scripts/dogfood-review-loop.py full     # bootstrap+compile+
#                                                  # reconcile+trace+emit
#   python scripts/dogfood-review-loop.py verify   # fail if the kernel
#                                                  # state and the doc's
#                                                  # compiled section drift
#   python scripts/dogfood-review-loop.py trace    # print trace answers
#
# Options: --kb DIR (default ./kb), --trace-check "id:expected-token"
# (repeatable; verify and trace use it; the default checks are the
# FINDINGS table's pinned answers).
import argparse
import asyncio
import base64
import json
import os
import pathlib
import re
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
DOC = REPO / "docs" / "PR6-TDD-DISPOSITIONS.md"
STATE_FILE = REPO / "docs" / "PR6-TDD-DISPOSITIONS.state.json"
BEGIN, END = "<!-- DOGFOOD-COMPILED-BEGIN -->", "<!-- DOGFOOD-COMPILED-END -->"

FINDINGS = [
    ("R3-001", "NOT A FINDING", "closed", "8e5dd8e",
     "check-no-tracked-node-modules.sh proves 0 tracked on every CI run"),
    ("R3-002", "FIXED", "closed", "8e5dd8e",
     "check-estate-hygiene.sh — RED vs origin/main (11 paths), GREEN vs HEAD"),
    ("R3-003", "FIXED", "closed", "a70773f",
     "sfm009 RED 192700 KiB → streamed validation GREEN"),
    ("R3-004", "COVERED", "closed", "f291130",
     "GREEN pin; structural RED = PR6-001's ckp009"),
    ("R3-005", "FIXED", "closed", "HEAD",
     "check-disposition-head.sh fails any move of the reviewed tip "
     "without a re-stamp"),
    ("R2-008", "CLOSED", "closed", "7577e94",
     "verified non-reproducing; threads resolved after the next push "
     "re-runs the scan green"),
]
DEFAULT_CHECKS = [("R3-003", "finding: R3-003"),
                  ("R3-005", "re-stamping")]


def sh(args):
    out = subprocess.run(args, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"{args}: {out.stderr.strip()}")
    return out.stdout.strip()


def read_state():
    """The machine state sidecar: {doc_koid, kos: {id: koid}, head, ...}.

    A sidecar, not a doc comment: the compiled section must stay clean for
    the compiler, or trace_requirement matches the state blob instead of
    the findings table (the dogfood's own self-reference catch)."""
    try:
        return json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def emit_state(state):
    head = sh(["git", "rev-parse", "HEAD"])
    state["head"] = head
    rows = []
    for fid, disp, status, fix, ev in FINDINGS:
        k = state["kos"].get(fid, "-")
        rows.append(f"| {fid} | {k} | {disp} | {status} |")
    blob = (
        f"{BEGIN}\n"
        "## Compiled from kernel state (P1-8 dogfood)\n\n"
        "`scripts/dogfood-review-loop.py full` emits this section from the\n"
        "project knowledge base (`./kb`, served by the repo's own aikoql-mcp\n"
        "plugin; machine state lives in the `.state.json` sidecar). The\n"
        "findings are Requirement KOs; the dispositions doc is the compiled\n"
        "knowledge document; each re-stamp commit below is reconciled via\n"
        "the A8 `reconcile` tool against that document.\n\n"
        f"- knowledge document KOID: `{state['doc_koid']}`\n"
        f"- reconciled re-stamp commits: {len(state['reconciled'])} "
        f"(first {state['reconciled'][0][:7]}, last {state['reconciled'][-1][:7]})\n"
        f"- trace answers: `{state['traces']}`\n"
        "\nThe trace pins the requirement leg (the finding is found by query).\n"
        "The tests leg is empty by construction today — two extractor gaps the\n"
        "dogfood itself surfaced: the markdown compiler attaches mock tokens,\n"
        "not component names, as fact entities, and the code extractor's\n"
        "`tested_by` objects are `crate`, which the tests-leg walk\n"
        "(components/functions) cannot match. Closing those is MRFC-0070\n"
        "follow-up, not this milestone.\n\n"
        "| finding | KOID | disposition | status |\n|---|---|---|---|\n"
        + "\n".join(rows) + "\n\n"
        f"compiled-head: {head}\n"
        f"{END}\n"
    )
    text = DOC.read_text(encoding="utf-8")
    if BEGIN in text:
        text = re.sub(rf"{BEGIN}.*?{END}\n?", blob, text, flags=re.DOTALL)
    else:
        text = text.rstrip() + "\n\n" + blob
    DOC.write_text(text, encoding="utf-8")
    STATE_FILE.write_text(json.dumps(state, indent=1) + "\n",
                          encoding="utf-8")
    return state


def tool_result(res):
    return "\n".join(
        getattr(c, "text", "") for c in res.content
        if getattr(c, "type", "text") == "text"
    )


def expect_ok(what, text):
    try:
        if json.loads(text).get("ok") is False:
            sys.exit(f"{what} rejected: {text[:300]}")
    except json.JSONDecodeError:
        pass  # plain-text result, not the ok-envelope
    return text


async def run(opts, checks):
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client

    params = StdioServerParameters(
        command="npx",
        args=["-y", "aikoql-mcp", "serve", opts.kb],
        cwd=str(REPO),
    )
    code, err = 0, None
    async with stdio_client(params) as (r, w):
        async with ClientSession(r, w) as s:
            await s.initialize()
            try:
                code = await dispatch(s, opts, checks)
            except BaseException as e:
                err = e
    # Re-raise only after the session closed cleanly: an in-flight exit
    # aborts the stdio close and the store can lose the run's writes.
    if err is not None:
        raise err
    return code


async def dispatch(s, opts, checks):
    if opts.op == "verify":
        return await verify(s, opts, checks)
    if opts.op == "trace":
        return await do_trace(s, opts, checks)
    state = await bootstrap(s)
    state = await compile_doc(s, state)
    state = await reconcile(s, state)
    state = await do_trace(s, opts, checks, state)
    state = emit_state(state)
    print("emitted compiled section (head %s)" % state["head"])
    return 0


async def bootstrap(s, state=None):
    state = state or read_state() or {"doc_koid": None, "kos": {}, "reconciled": []}
    for fid, disp, status, fix, ev in FINDINGS:
        props = {"id": fid, "title": f"PR6 Round-3 finding {fid}",
                 "disposition": disp, "status": status,
                 "fix_commit": fix, "evidence": ev}
        if fid in state["kos"]:
            res = await s.call_tool("get", {
                "koid": state["kos"][fid], "subject": "dogfood",
            })
            if '"ok":false' in tool_result(res):
                state["kos"].pop(fid)  # stale KOID (kb rebuilt) → recreate
        if fid in state["kos"]:
            res = await s.call_tool("remember", {
                "type_name": "Requirement", "koid": state["kos"][fid],
                "properties": props, "subject": "dogfood",
                "note": "P1-8 review-loop dogfood (upsert)",
            })
        else:
            res = await s.call_tool("remember", {
                "type_name": "Requirement", "properties": props,
                "subject": "dogfood", "note": "P1-8 review-loop dogfood",
            })
        koid = json.loads(tool_result(res)).get("koid")
        if not koid:
            sys.exit(f"bootstrap: no koid for {fid}: {tool_result(res)[:200]}")
        state["kos"][fid] = koid
    print("bootstrap: %d Requirement KOs upserted" % len(FINDINGS))
    return state


async def compile_doc(s, state):
    text = DOC.read_text(encoding="utf-8")
    if BEGIN in text:
        # Compile the doc WITHOUT the previous run's compiled section: it
        # is emitted FROM kernel state, so compiling it back in makes
        # trace_requirement first-match the stale blob — the dogfood's own
        # self-reference catch, again.
        text = re.sub(rf"{BEGIN}.*?{END}\n?", "", text, flags=re.DOTALL)
    b64 = base64.b64encode(text.encode("utf-8")).decode()
    res = await s.call_tool("document_ingest", {
        "filename": "PR6-TDD-DISPOSITIONS.md", "content_base64": b64,
        "mime_type": "text/markdown", "subject": "dogfood",
    })
    koid = json.loads(expect_ok("document_ingest", tool_result(res))).get("koid")
    if not koid:
        sys.exit(f"document_ingest: no koid: {tool_result(res)[:200]}")
    res = await s.call_tool("document_compile", {"koid": koid,
                                                 "subject": "dogfood"})
    out = expect_ok("document_compile", tool_result(res))
    print("document_compile: %s" % out.splitlines()[0] if out else "done")
    state["doc_koid"] = koid
    return state


async def reconcile(s, state):
    commits = sh(["git", "log", "--format=%H", "--", "docs/PR6-TDD-DISPOSITIONS.md"]).split()
    state["reconciled"] = commits
    for sha in commits:
        files = sh(["git", "diff-tree", "--no-commit-id", "--name-only",
                    "-r", sha]).split()
        res = await s.call_tool("reconcile", {
            "koid": state["doc_koid"], "files": files, "subject": "dogfood",
        })
        text = expect_ok("reconcile", tool_result(res))
        first = text.splitlines()[0] if text else "(no output)"
        print(f"reconcile {sha[:7]}: {first}")
    print("reconciled %d re-stamp commits" % len(commits))
    return state


async def do_trace(s, opts, checks, state=None):
    if state is None:
        state = read_state()
        if not state or not state.get("doc_koid"):
            sys.exit("trace: no compiled knowledge document — run `full` first")
    answers = {}
    for rid, token in checks:
        res = await s.call_tool("trace_requirement", {
            "koid": state["doc_koid"], "requirement": rid,
            "subject": "dogfood",
        })
        text = tool_result(res)
        answers[rid] = text
        if '"ok":false' in text:
            sys.exit(f"trace {rid} rejected: {text[:300]}")
        # Anti-echo: the pin must come from the findings themselves, not
        # from this loop's own emitted section or state (which repeat the
        # pin tokens by design).
        echo = "DOGFOOD-COMPILED" in text or '"traces"' in text
        ok = not echo and token.lower() in text.lower()
        print(f"trace_requirement({rid}): "
              f"{'OK' if ok else 'MISSING ' + token!r}"
              f"{' (echo)' if echo else ''} — "
              f"{text.splitlines()[0] if text else '(no output)'}")
        if not ok:
            sys.exit(f"trace check failed: {rid} does not name {token}")
    if state is not None:
        state["traces"] = " | ".join(
            f"{rid}->{t}" for rid, t in checks)
    return state


async def verify(s, opts, checks):
    state = read_state()
    if not state:
        sys.exit("verify: no state sidecar "
                 "(docs/PR6-TDD-DISPOSITIONS.state.json) — run `full` first")
    head = sh(["git", "rev-parse", "HEAD"])
    if state.get("head") != head:
        sys.exit(f"verify: compiled-head {state.get('head')[:7]} != HEAD "
                 f"{head[:7]} — re-run `full`")
    for fid, disp, status, fix, ev in FINDINGS:
        koid = state["kos"].get(fid)
        if not koid:
            sys.exit(f"verify: no KOID recorded for {fid}")
        res = await s.call_tool("get", {"koid": koid, "subject": "dogfood"})
        got = json.loads(tool_result(res))
        if got.get("type_name") != "Requirement" or \
                got.get("properties", {}).get("id") != fid:
            sys.exit(f"verify: KO {koid} is not {fid}: "
                     f"{tool_result(res)[:200]}")
    await do_trace(s, opts, checks)
    print("verify: %d findings as KOs, traces pinned, compiled-head == HEAD"
          % len(FINDINGS))
    return 0


def main():
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("op", nargs="?", default="full",
                    choices=["full", "verify", "trace"])
    ap.add_argument("--kb", default=str(REPO / "kb"))
    ap.add_argument("--trace-check", action="append", default=None,
                    metavar="ID:TOKEN",
                    help="verify/trace must see TOKEN in trace_requirement(ID)")
    opts = ap.parse_args()
    checks = DEFAULT_CHECKS
    if opts.trace_check:
        checks = [tuple(c.split(":", 1)) for c in opts.trace_check]
    sys.exit(asyncio.run(run(opts, checks)))


if __name__ == "__main__":
    main()
