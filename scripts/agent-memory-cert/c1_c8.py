"""AikoQL as agent memory: the C1-C8 levels of
docs/AIKOQL-Chatbot-Memory-Knowledge-Certification-Test-Suite.md, driven
black-box over MCP (TCP transport), no LLM in the loop.

Each check returns PASS / FAIL / GAP (the capability is not there) with the
server's own answer as evidence. Results: results.json + a printed table.
Usage: python scripts/agent-memory-cert/c1_c8.py
  AIKOQL_BIN       the server binary (default target/debug/aikoql-mcp)
  AIKOQL_CERT_OUT  work dir for the databases, logs and results.json
                   (default <tmp>/aikoql-agent-memory-cert)"""
import json
import pathlib
import shutil
import sys
import time
import traceback

EV = lambda src: [{"source_artifact": src, "method": "human_provided"}]

from mcp import Server, Session, BIN, HERE
DB = HERE / "mem-db"
LOG = HERE / "server.log"
TOKENS = ["acme-agent:acme:admin", "acme-bob:acme:editor", "acme-viewer:acme:viewer",
          "globex-agent:globex:admin"]

results = []


def check(level, cid, title):
    def wrap(fn):
        def run(ctx):
            CURRENT[0] = cid
            t = time.time()
            try:
                verdict, evidence = fn(ctx)
            except Exception as e:  # a harness or transport failure is a FAIL with the trace
                verdict, evidence = "FAIL", f"{type(e).__name__}: {e} | {traceback.format_exc(limit=2)[-300:]}"
            results.append({"level": level, "id": cid, "title": title, "verdict": verdict,
                            "semantic": SEMANTIC.get(cid),
                            "evidence": str(evidence)[:600], "secs": round(time.time() - t, 2)})
            print(f"{level} {cid:<16} {verdict:<4} {title} :: {str(evidence)[:160]}", flush=True)
        CHECKS.append(run)
        return fn
    return wrap


CHECKS = []
CURRENT = [""]


def koid_of(data):
    return data.get("koid") if isinstance(data, dict) else None


def prices(q):
    return sorted(r.get("properties", {}).get("price") for r in (q.get("results") or [])
                  if isinstance(q, dict))


SEMANTIC = {}  # check id -> whether recall's semantic leg ran


def recall_text(s, query, **kw):
    ok, d = s.tool("recall", query=query, k=kw.get("k", 8), wait_for_freshness_ms=3000)
    assert ok, f"recall failed: {d}"
    SEMANTIC.setdefault(CURRENT[0], []).append(bool(d.get("semantic")))
    return d, json.dumps(d, ensure_ascii=False).lower()


# --------------------------------------------------------------- C1 conversational
@check("C1", "CHAT-MEM-001", "same-session memory")
def _(c):
    s = c["a"]
    ok, d = s.tool("remember", type_name="Preference", embed=True,
                   properties={"user": "alice", "key": "language", "value": "English",
                               "text": "Alice's preferred language is English"})
    c["pref_lang"] = koid_of(d)
    if not ok:
        return "FAIL", d
    _, t = recall_text(s, "What language does Alice prefer?")
    return ("PASS" if "english" in t else "FAIL"), t[:300]


@check("C1", "CHAT-MEM-002", "cross-session memory")
def _(c):
    s = c["a"]
    s.tool("remember", type_name="Preference", embed=True,
           properties={"user": "alice", "key": "style", "value": "concise",
                       "text": "Alice prefers concise answers"})
    s2 = Session(c["srv"], "acme-agent")
    try:
        _, t = recall_text(s2, "How does Alice like her answers?")
    finally:
        s2.close()
    return ("PASS" if "concise" in t else "FAIL"), t[:300]


@check("C1", "CHAT-MEM-003", "memory survives a restart")
def _(c):
    c["a"].close()
    c["srv"].stop()
    c["srv"] = Server(DB, TOKENS, LOG)
    c["a"] = Session(c["srv"], "acme-agent")
    _, t = recall_text(c["a"], "What language does Alice prefer?")
    return ("PASS" if "english" in t else "FAIL"), t[:300]


@check("C1", "CHAT-MEM-004", "explicit memory (memory_store)")
def _(c):
    s = c["a"]
    ok, d = s.tool("memory_store", name="alice-deploy-env",
                   description="Alice's preferred deployment environment",
                   content="Alice's preferred deployment environment is AWS.",
                   type="user", memory_dir=str(HERE / "memdir"))
    if not ok:
        return "FAIL", d
    ok, d = s.tool("memory_search", query="deployment environment",
                   memory_dir=str(HERE / "memdir"))
    t = json.dumps(d).lower()
    return ("PASS" if ok and "aws" in t else "FAIL"), t[:300]


@check("C1", "CHAT-MEM-005", "ephemeral statement expires (TTL)")
def _(c):
    s = c["a"]
    ok, d = s.tool("agent_memory", agent_id="assistant-1", key="current_test_env",
                   value="AWS (testing right now)", ttl=2)
    if not ok:
        return "FAIL", d
    ok, before = s.tool("agent_memory", agent_id="assistant-1")
    time.sleep(3.5)
    ok2, after = s.tool("agent_memory", agent_id="assistant-1")
    held = "current_test_env" in json.dumps(before)
    gone = "current_test_env" not in json.dumps(after)
    return ("PASS" if held and gone else "FAIL"), f"before={str(before)[:150]} after={str(after)[:150]}"


# --------------------------------------------------------------- C2 semantic
@check("C2", "SEM-001", "direct fact, deterministic query (no LLM)")
def _(c):
    s = c["a"]
    ok, d = s.tool("remember", type_name="Product", embed=True,
                   properties={"name": "Starter", "price": 10, "currency": "EUR",
                               "text": "The Starter plan costs 10 EUR per month"})
    c["price"] = koid_of(d)
    ok, q = s.tool("aikoql", query='MATCH Product WHERE name == "Starter" RETURN price')
    t = json.dumps(q)
    return ("PASS" if ok and "10" in t else "FAIL"), t[:300]


@check("C2", "SEM-002", "multi-hop knowledge (graph traversal)")
def _(c):
    s = c["a"]
    _, p = s.tool("remember", type_name="Person", properties={"name": "Ravi"})
    _, t = s.tool("remember", type_name="Team", properties={"name": "Payments"})
    _, v = s.tool("remember", type_name="Service", properties={"name": "ledger-api"})
    s.tool("relate", **{"from": koid_of(p), "to": koid_of(t), "rel_type": "MEMBER_OF"})
    s.tool("relate", **{"from": koid_of(t), "to": koid_of(v), "rel_type": "OWNS"})
    ok, d = s.tool("traverse", koid=koid_of(p), depth=2)
    txt = json.dumps(d)
    return ("PASS" if ok and koid_of(v) in txt else "FAIL"), txt[:300]


@check("C2", "TEMP-CHAT-001", "current truth after a change (supersede)")
def _(c):
    s = c["a"]
    c["t_before"] = int(time.time() * 1000)
    time.sleep(1.1)
    ok, d = s.tool("supersede", old=c["price"], type_name="Product",
                   properties={"name": "Starter", "price": 12, "currency": "EUR",
                               "text": "The Starter plan costs 12 EUR per month"},
                   evidence=EV("pricing page 2026-11"), reason="price change")
    if not ok:
        return "FAIL", d
    c["price2"] = d.get("new") if isinstance(d, dict) else None  # supersede answers {old, new}
    ok, q = s.tool("aikoql", query='MATCH Product WHERE name == "Starter" RETURN price')
    return ("PASS" if ok and prices(q) == [12] else "FAIL"), f"current prices {prices(q)}"


@check("C2", "TEMP-CHAT-002", "historical truth (AS_OF before the change)")
def _(c):
    s = c["a"]
    ok, q = s.tool("aikoql", query=f'MATCH Product AS_OF {c["t_before"]} WHERE name == "Starter" RETURN price')
    return ("PASS" if ok and prices(q) == [10] else "FAIL"), f"prices as of before the change {prices(q)}"


@check("C2", "CONTR-CHAT-001", "contradiction is recorded, not overwritten")
def _(c):
    s = c["a"]
    _, d = s.tool("remember", type_name="Policy", properties={"name": "refund-window", "days": 30,
                  "text": "Refunds are accepted within 30 days"})
    c["claim"] = koid_of(d)
    ok, d = s.tool("contradict", claim=c["claim"], properties={"name": "refund-window", "days": 14,
                   "text": "Refunds are accepted within 14 days"}, authority="organization_policy",
                   evidence=EV("legal update"))
    c["conflict"] = (d or {}).get("conflict") if isinstance(d, dict) else None
    return ("PASS" if ok and c["conflict"] else "FAIL"), str(d)[:300]


@check("C2", "CONTR-CHAT-002", "authoritative source wins (resolve by authority)")
def _(c):
    if not c.get("conflict"):
        return "FAIL", "no conflict to resolve"
    ok, d = c["a"].tool("resolve_conflict_by_authority", koid=c["conflict"], rationale="legal outranks wiki")
    return ("PASS" if ok else "FAIL"), str(d)[:300]


@check("C2", "FORGET-001", "forget removes a memory from recall")
def _(c):
    s = c["a"]
    _, d = s.tool("remember", type_name="Note", embed=True,
                  properties={"text": "Alice's old phone number is 555-0199 zebrafish"})
    k = koid_of(d)
    _, before = recall_text(s, "Alice old phone number zebrafish")
    ok, f = s.tool("forget", koid=k)
    _, after = recall_text(s, "Alice old phone number zebrafish")
    return ("PASS" if ok and k in before and k not in after else "FAIL"), f"in_before={k in before} in_after={k in after} forget={str(f)[:120]}"


# --------------------------------------------------------------- C3 episodic
@check("C3", "EP-001", "a past interaction is recalled as an episode")
def _(c):
    s = c["a"]
    ok, d = s.tool("record_experience", goal="deploy ledger-api to staging",
                   action="ran the blue-green deploy runbook", outcome="success",
                   evidence=EV("deploy log 2026-11-12"), lesson="drain the old pool first",
                   reuse_conditions=["deploy", "staging"], confidence=0.9)
    c["ep1"] = koid_of(d)
    if not ok:
        return "FAIL", d
    ok, f = s.tool("find_experiences", task="deploy a service to staging")
    t = json.dumps(f)
    return ("PASS" if ok and (c["ep1"] or "x") in t else "FAIL"), t[:300]


@check("C3", "CONS-004", "a failed action is recorded and surfaced")
def _(c):
    s = c["a"]
    ok, d = s.tool("record_experience", goal="deploy ledger-api to production",
                   action="deployed without a database migration", outcome="failure",
                   evidence=EV("incident INC-42"), lesson="run migrations before the deploy",
                   reuse_conditions=["deploy", "production"], confidence=0.8)
    k = koid_of(d)
    ok2, f = s.tool("find_experiences", task="deploy ledger-api to production")
    t = json.dumps(f).lower()
    return ("PASS" if ok and ok2 and (k or "x") in t and "failure" in t else "FAIL"), t[:300]


@check("C3", "CONS-001", "conversation consolidated into a summary episode")
def _(c):
    t0 = int(time.time() * 1000)
    msgs = [{"speaker": "alice", "ts_ms": t0, "text": "We decided to move the ledger to Postgres 16."},
            {"speaker": "bot", "ts_ms": t0 + 1000, "text": "Noted. The migration must finish before Friday."},
            {"speaker": "alice", "ts_ms": t0 + 2000, "text": "Ravi will own the migration."}]
    ok, d = c["a"].tool("summarize_conversation", conversation_id="conv-1", messages=msgs,
                        evidence=EV("chat transcript conv-1"))
    ok2, g = c["a"].tool("get", koid=koid_of(d))  # the summary KO itself
    t = json.dumps(g).lower()
    return ("PASS" if ok and "postgres 16" in t and "ravi" in t else "FAIL"), t[:300]


@check("C3", "EP-004", "episode provenance (where a memory came from)")
def _(c):
    ok, d = c["a"].tool("provenance", koid=c["ep1"])
    t = json.dumps(d).lower()
    return ("PASS" if ok and "deploy log 2026-11-12" in t else "FAIL"), t[:300]


# --------------------------------------------------------------- C4 procedural
@check("C4", "PROC-CHAT-001", "a known procedure is recalled")
def _(c):
    s = c["a"]
    ok, d = s.tool("remember", type_name="Procedure", embed=True,
                   properties={"name": "rotate-api-key", "version": 1,
                               "text": "To rotate an API key: create the new key, deploy it, then revoke the old key"})
    c["proc"] = koid_of(d)
    _, t = recall_text(s, "How do I rotate an API key?")
    return ("PASS" if (c["proc"] or "x") in t else "FAIL"), t[:300]


@check("C4", "PROC-CHAT-002", "procedure versioning keeps the new and the old")
def _(c):
    s = c["a"]
    ok, d = s.tool("supersede", old=c["proc"], type_name="Procedure",
                   properties={"name": "rotate-api-key", "version": 2,
                               "text": "To rotate an API key: create the new key, deploy it, wait 24 hours, then revoke the old key"},
                   evidence=EV("security review 2026-11"), reason="add a grace period")
    if not ok:
        return "FAIL", d
    _, t = recall_text(s, "How do I rotate an API key?")
    ok2, old = s.tool("get", koid=c["proc"])
    o = json.dumps(old).lower()
    return ("PASS" if "24 hours" in t and "superseded" in o else "FAIL"), f"recall={t[:150]} old={o[:150]}"


# --------------------------------------------------------------- C5 knowledge-grounded
@check("C5", "PROV-CHAT-001", "recall says where a memory came from (evidence in why)")
def _(c):
    d, t = recall_text(c["a"], "What does the Starter plan cost per month?")
    items = [i for i in d.get("items", []) if "starter" in json.dumps(i).lower()]
    why = items[0].get("why", "") if items else ""
    return ("PASS" if "pricing page 2026-11" in why else "FAIL"), f"why={why!r}"


# --------------------------------------------------------------- C6 personalized
@check("C6", "PERS-002", "preference provenance (who said it)")
def _(c):
    ok, d = c["a"].tool("provenance", koid=c["pref_lang"])
    t = json.dumps(d).lower()
    return ("PASS" if ok and "origin" in t and ("acme-agent" in t or "agent(" in t) else "FAIL"), t[:400]


@check("C6", "PERS-003", "a changed preference: the newer one is current")
def _(c):
    s = c["a"]
    ok, d = s.tool("supersede", old=c["pref_lang"], type_name="Preference",
                   properties={"user": "alice", "key": "language", "value": "German",
                               "text": "Alice's preferred language is German"},
                   evidence=EV("alice, chat 2026-11-13"), reason="user changed preference")
    _, t = recall_text(s, "What language does Alice prefer?")
    return ("PASS" if ok and "german" in t else "FAIL"), t[:300]


@check("C6", "PERS-004", "preference scope: another tenant never sees it")
def _(c):
    g = Session(c["srv"], "globex-agent")
    try:
        _, t = recall_text(g, "What language does Alice prefer?")
    finally:
        g.close()
    leak = "german" in t or "english" in t
    return ("FAIL" if leak else "PASS"), t[:300]


@check("C6", "PERS-005", "a private memory: another user in the same tenant never sees it")
def _(c):
    s = c["a"]
    ok, d = s.tool("memory_store", name="alice-salary-band", description="Alice's salary band",
                   content="Alice is in salary band L7.", type="user")
    if not ok:
        return "FAIL", d
    b = Session(c["srv"], "acme-bob")
    try:
        _, mine = s.tool("memory_search", query="salary band")
        _, his = b.tool("memory_search", query="salary band")
    finally:
        b.close()
    seen_by_owner = "salary" in json.dumps(mine).lower()
    leak = "salary" in json.dumps(his).lower()
    if seen_by_owner and leak:
        # every TCP token is the agent "tcp-agent" (session_init refuses an
        # agent_id over TCP): two agents in one tenant are one principal
        return "GAP", "two TCP tokens in one tenant are the same agent ('tcp-agent'): per-agent private memory is not expressible over TCP"
    return ("PASS" if seen_by_owner and not leak else "FAIL"), f"owner sees={seen_by_owner} other user sees={leak}"


# --------------------------------------------------------------- C7 constraint-aware# --------------------------------------------------------------- C7 constraint-aware
@check("C7", "AUTH-001", "a viewer cannot write memory")
def _(c):
    v = Session(c["srv"], "acme-viewer")
    try:
        ok, d = v.tool("remember", type_name="Note", properties={"text": "viewer write"})
    finally:
        v.close()
    return ("PASS" if not ok else "FAIL"), str(d)[:300]


@check("C7", "SENS-001", "a secret stored in memory is not handed back raw")
def _(c):
    s = c["a"]
    s.tool("remember", type_name="Note", embed=True,
           properties={"text": "Alice's AWS key is AKIAIOSFODNN7EXAMPLE, keep it safe"})
    _, t = recall_text(s, "What is Alice's AWS key?")
    raw = "akiaiosfodnn7example" in t
    return ("FAIL" if raw else "PASS"), ("the raw key is returned by recall: " if raw else "") + t[:300]


@check("C7", "POLICY-001", "a deny policy blocks the action itself")
def _(c):
    s = c["a"]
    s.tool("deploy_policy", name="NoAgentDeletes", effect="Deny", principal="acme-agent",
           action="Delete", resource_type="Note")
    ok, ev = s.tool("evaluate_policies", principal="acme-agent", action="Delete", resource_type="Note")
    _, d = s.tool("remember", type_name="Note", properties={"text": "a note the agent may not delete"})
    ok2, f = s.tool("forget", koid=koid_of(d))
    evaluated = '"allowed": false' in json.dumps(ev).lower()
    if evaluated and ok2:
        return "GAP", "policies are advisory: evaluate_policies says denied, the forget itself still succeeds"
    return ("PASS" if evaluated and not ok2 else "FAIL"), f"evaluate={str(ev)[:100]} forget_ok={ok2} {str(f)[:100]}"


# --------------------------------------------------------------- C8 agentic# --------------------------------------------------------------- C8 agentic
@check("C8", "PROG-CHAT-001", "a program is a versioned memory object and runs")
def _(c):
    s = c["a"]
    ok, d = s.tool("deploy_program", name="StarterPrice",
                   body='MATCH Product WHERE name == "Starter" RETURN price', language="aikoql")
    c["prog"] = koid_of(d)
    ok2, r = s.tool("execute_program", koid=c["prog"], execution_id="run-1")
    return ("PASS" if ok and ok2 and prices(r) == [12] else "FAIL"), f"program result prices {prices(r)}"


@check("C8", "PROG-CHAT-004", "exactly-once execution: a replay returns the snapshot even after the data changed")
def _(c):
    s = c["a"]
    cur = c.get("price2") or c["price"]
    ok, d = s.tool("supersede", old=cur, type_name="Product",
                   properties={"name": "Starter", "price": 13, "currency": "EUR",
                               "text": "The Starter plan costs 13 EUR per month"},
                   evidence=EV("pricing page 2026-12"), reason="second price change")
    _, replay = s.tool("execute_program", koid=c["prog"], execution_id="run-1")
    _, fresh = s.tool("execute_program", koid=c["prog"], execution_id="run-2")
    return ("PASS" if ok and prices(replay) == [12] and prices(fresh) == [13] else "FAIL"), \
        f"replay run-1 {prices(replay)} (expect [12]); new run-2 {prices(fresh)} (expect [13])"


@check("C8", "EVOL-001", "validated knowledge evolves (lifecycle)")
def _(c):
    s = c["a"]
    ok, d = s.tool("evolve", koid=c["ep1"], to="active")
    ok2, d2 = s.tool("evolve", koid=c["ep1"], to="verified")
    ok3, g = s.tool("get", koid=c["ep1"])
    t = json.dumps(g).lower()
    return ("PASS" if ok2 and "verified" in t else "FAIL"), f"{str(d)[:100]} | {str(d2)[:100]} | {t[:120]}"


def main():
    for side in [DB, *HERE.glob("mem-db.*")]:  # the db and its sidecars (.ckpt, .artifacts, logs)
        if side.is_dir():
            shutil.rmtree(side, ignore_errors=True)
        else:
            side.unlink(missing_ok=True)
    shutil.rmtree(HERE / "memdir", ignore_errors=True)
    LOG.write_text("")
    ctx = {"srv": Server(DB, TOKENS, LOG)}
    ctx["a"] = Session(ctx["srv"], "acme-agent")
    print("binary:", BIN)
    try:
        for run in CHECKS:
            run(ctx)
    finally:
        ctx["srv"].stop()
    (HERE / "results.json").write_text(json.dumps(results, indent=1, ensure_ascii=False), encoding="utf-8")
    print("\nLEVEL  PASS FAIL GAP")
    for lvl in sorted({r["level"] for r in results}):
        rs = [r for r in results if r["level"] == lvl]
        print(f"{lvl:<6} {sum(r['verdict']=='PASS' for r in rs):>4} {sum(r['verdict']=='FAIL' for r in rs):>4} {sum(r['verdict']=='GAP' for r in rs):>3}")


if __name__ == "__main__":
    sys.exit(main())
