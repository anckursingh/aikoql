import json, sys, urllib.request, urllib.parse
BASE = "http://127.0.0.1:9494"
tok = json.load(urllib.request.urlopen(urllib.request.Request(BASE+"/api/login", data=json.dumps({"username":"admin","password":"studio-demo"}).encode(), headers={"Content-Type":"application/json"})))["token"]
def call(method, path, body=None):
    req = urllib.request.Request(BASE+path, method=method, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization":"Bearer "+tok,"X-Aikoql-Csrf":"1","Content-Type":"application/json"})
    try:
        return json.load(urllib.request.urlopen(req, timeout=300))
    except urllib.error.HTTPError as e:
        return {"HTTP": e.code, "body": e.read().decode()[:300]}
def q(text): return call("GET", "/api/aikoql?query="+urllib.parse.quote(text))
def rows(r): return r.get("results", [])
def show(title, v, n=700): print("\n## "+title+"\n"+(v if isinstance(v,str) else json.dumps(v))[:n])

for s in ("device-intel", "crm"):
    d = call("POST", "/api/v1/sources/discover", {"source": s}).get("data", {})
    show("discover "+s, {k: d.get(k) for k in ("containers","references","truncated","changed")})
p = call("POST", "/api/v1/sources/ontology", {"sources": ["device-intel","crm"], "preview": True}).get("data", {})
show("preview classes", [(c["name"], c["sources"]) for c in p.get("classes", [])])
show("preview suggestions", p.get("suggestions"))
show("preview warnings", p.get("warnings"))
pub = call("POST", "/api/v1/sources/ontology", {"sources": ["device-intel","crm"], "preview": False, "fingerprints": p.get("fingerprints")})
show("publish", {k: pub.get("data", pub).get(k) for k in ("saved","version")})
imp = call("POST", "/api/v1/sources/import", {"source": "device-intel", "containers": ["devices","device_activity","risk_decisions"]}).get("data", {})
show("import device-intel", {k: imp.get(k) for k in ("imported","truncated","skipped")})
imp = call("POST", "/api/v1/sources/import", {"source": "crm", "containers": ["accounts"]}).get("data", {})
show("import crm", {k: imp.get(k) for k in ("imported","truncated","skipped")})
json.dump({"ok": True}, open(sys.argv[1], "w"))
