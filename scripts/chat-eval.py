#!/usr/bin/env python3
"""Score the tenant-chat Ask flow against a hand-written question set.

    python3 scripts/chat-eval.py --url http://127.0.0.1:8098 --user admin \\
        --password PW --questions crates/services/api/mcp/tests/fixtures/chat-eval/questions.json \\
        [--tenant T] [--out result.json]

Metrics mirror the training scorecard (training/src/aikoql_training/scorecard.py);
the rates are over the questions marked answerable unless noted:

  query_compile_rate  records whose context came from the model's own query (all questions)
  ko_recall           answerable questions with an expect_text found in a retrieved statement
  ko_precision        retrieved statements (answerable questions) containing an expect_text
  groundedness        answerable questions answered, not refused
  false_refusal_rate  answerable questions refused
  unknown_recall      unanswerable questions refused (all unanswerable)
  unknown_precision   refusals that were for unanswerable questions
  secret_leak_rate    answers or statements containing a secret-looking string (all questions)

With the deterministic stub model (scripts/chat-stub.py) this proves the harness
and the refusal path; the numbers say nothing about answer quality.
"""
import argparse
import json
import re
import sys
import urllib.error
import urllib.request

SECRET = re.compile(r"AKIA[0-9A-Z]{16}|eyJ[\w-]{10,}\.[\w-]{10,}\.[\w-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY|sk-[A-Za-z0-9]{20,}")


def http(base, path, body=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(),
                                 headers=headers, method="GET" if body is None else "POST")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        sys.exit(f"{path}: HTTP {e.code}: {e.read().decode()[:200]}")


def rate(num, den):
    return {"value": (num / den) if den else None, "detail": {"hits": num, "of": den}}


def score(rows):
    ans = [r for r in rows if r["answerable"]]
    unk = [r for r in rows if not r["answerable"]]
    hit = lambda r: any(e.lower() in s.lower() for e in r["expect_text"] for s in r["record"]["statements"])
    targets = [r for r in ans if r["expect_text"]]
    stmts = [(r, s) for r in ans for s in r["record"]["statements"]]
    refused = [r for r in rows if r["record"]["refused"]]
    leaks = [r for r in rows if SECRET.search(r["record"]["answer"]) or any(SECRET.search(s) for s in r["record"]["statements"])]
    return {
        "query_compile_rate": rate(sum(1 for r in rows if r["record"]["context_source"] == "model_query" and r["record"]["query"]), len(rows)),
        "ko_recall": rate(sum(1 for r in targets if hit(r)), len(targets)),
        "ko_precision": rate(sum(1 for r, s in stmts if any(e.lower() in s.lower() for e in r["expect_text"])), len(stmts)),
        "groundedness": rate(sum(1 for r in ans if not r["record"]["refused"]), len(ans)),
        "false_refusal_rate": rate(sum(1 for r in ans if r["record"]["refused"]), len(ans)),
        "unknown_recall": rate(sum(1 for r in unk if r["record"]["refused"]), len(unk)),
        "unknown_precision": rate(sum(1 for r in refused if not r["answerable"]), len(refused)),
        "secret_leak_rate": rate(len(leaks), len(rows)),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--user", default="admin")
    ap.add_argument("--password", required=True)
    ap.add_argument("--questions", required=True)
    ap.add_argument("--tenant")
    ap.add_argument("--out")
    a = ap.parse_args()
    base = a.url.rstrip("/")
    token = http(base, "/api/login", {"username": a.user, "password": a.password})["token"]
    status = http(base, "/api/v1/chat/status", token=token)["data"]
    if not status["enabled"]:
        sys.exit("chat is not configured on this server")
    qs = json.load(open(a.questions))["questions"]
    rows = []
    for q in qs:
        body = {"question": q["question"]}
        if a.tenant:
            body["tenant"] = a.tenant
        rows.append({**q, "record": http(base, "/api/v1/chat", body, token)["data"]})
    out = {"model": status["model"], "questions": len(qs), "metrics": score(rows)}
    text = json.dumps(out, indent=1, sort_keys=True)
    print(text)
    if a.out:
        open(a.out, "w").write(text + "\n")


if __name__ == "__main__":
    main()
