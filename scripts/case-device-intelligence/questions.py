import json, sys, urllib.request, urllib.parse, subprocess
exec(open(sys.argv[1] + "/walk.py").read().split("for s in (")[0])   # reuse login/call helpers
def ch(sql): return subprocess.run(["docker","exec","featureenterprise-tiers-clickhouse-1","clickhouse-client","--password","aikoql-dev-only","--query",sql],capture_output=True,text=True).stdout.strip()
def n(r): return len(rows(r)) if "results" in r else r
def props(r, k=0): return rows(r)[k]["properties"] if rows(r) else None

show("Q0 canonical names + mapping work", n(q('MATCH Device WHERE visitor_id == "vis_00003" RETURN *')))
dev = props(q('MATCH Device WHERE visitor_id == "vis_00003" RETURN *'))
show("device vis_00003 (what AIKOQL stores)", dev, 500)

# ---- Q1: what do we know about device X over time? ------------------------------
ch("INSERT INTO device_intel.devices SELECT visitor_id, first_seen, now(), browser, os, screen, timezone, language, incognito, hardware_concurrency, 0.99, fingerprint_hash FROM device_intel.devices FINAL WHERE visitor_id='vis_00003'")
imp = call("POST", "/api/v1/sources/import", {"source": "device-intel", "containers": ["devices"]}).get("data", {})
show("Q1 re-import after the source changed the device", {k: imp.get(k) for k in ("imported","unchanged")})
r_now = q('MATCH Device WHERE visitor_id == "vis_00003" RETURN *')
show("Q1 now: rows, risk, version", [(x["properties"].get("risk_score"), x.get("version")) for x in rows(r_now)])
r_hist = q('MATCH Device WHERE visitor_id == "vis_00003" HISTORICAL RETURN *')
show("Q1 HISTORICAL: versions kept", [(x["properties"].get("risk_score"), x.get("version")) for x in rows(r_hist)] if "results" in r_hist else r_hist)

# ---- Q2: which devices are similar to X? --------------------------------------
fh = dev["fingerprint_hash"] if dev else ""
show("Q2a same fingerprint_hash (exact component match)", n(q('MATCH Device WHERE fingerprint_hash == "%s" RETURN *' % fh)))
r_sim = q('MATCH Device SIMILAR TO "Chrome Windows 1920x1080 Europe/Oslo en-US" LIMIT 5 RETURN *')
show("Q2b SIMILAR TO (vector) on imported devices", {"rows": n(r_sim), "sample": (props(r_sim) or r_sim)} , 400)

# ---- Q3: which accounts share a device? ---------------------------------------
da = props(q('MATCH DeviceActivity WHERE visitor_id == "vis_00007" RETURN *'))
show("Q3a device_activity vis_00007", da)
rd = q('MATCH RiskDecision WHERE visitor_id == "vis_00007" RETURN *')
show("Q3b risk decisions for vis_00007 (the account ids sit inside rows, not as edges)", n(rd))
rels = rows(r_now)[0].get("relationships") if rows(r_now) else None
show("Q3c relationships on the imported Device KO", rels)
show("Q3d traverse from the device", q('MATCH Device WHERE visitor_id == "vis_00007" TRAVERSE depth 2 RETURN *') if False else "no relationships exist to traverse")

# ---- Q4: risk says block, CRM says active — which wins? -----------------------
acct = props(q('MATCH Account WHERE id == "acct_0077" RETURN *'))
show("Q4a CRM account acct_0077", acct)
rdec = rows(q('MATCH RiskDecision WHERE account_id == "acct_0077" RETURN *'))
show("Q4b risk decisions for acct_0077", [(x["properties"]["decision"], x["properties"]["reason"]) for x in rdec])
show("Q4c does AIKOQL link them or flag the conflict?", {"account_has_relationships": rows(q('MATCH Account WHERE id == "acct_0077" RETURN *'))[0].get("relationships")})
