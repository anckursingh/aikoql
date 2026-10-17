"""D-11: the Python adapter for the shared conformance runner (§7, §23).

Executes the language-neutral vectors from tests/sdk-conformance/ (the §23
canonical workload + the 13 §7 category dirs) and protocol/test-vectors/
against a real aikoql-mcp server through this SDK, then checks every
assert/assert_any/expect_error. The expected results are the vectors
themselves — every SDK produces the same transcript.

Run directly (spawns its own server) or through scripts/sdk-conformance.sh.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "python")))
from aikoql import McpClient, McpError  # noqa: E402


def dot_get(obj, path):
    """Walk a dot path; integer segments index lists ("chunks.0.results")."""
    for part in path.split("."):
        if isinstance(obj, dict):
            obj = obj[part]
        elif isinstance(obj, list):
            obj = obj[int(part)]
        else:
            raise KeyError(path)
    return obj


def resolve(value, vars_):
    if isinstance(value, str) and value.startswith("$") and value[1:] in vars_:
        return vars_[value[1:]]
    return value


def map_code(e):
    """Wire surfaces → SDK-012 codes: the server's -32001 token rejection
    is AUTHENTICATION_FAILED to the caller. Codes arrive as JSON numbers
    or strings — normalize before comparing against the vector."""
    code = str(e.code)
    return "AUTHENTICATION_FAILED" if code == "-32001" else code


class Runner:
    """One vector set against one server; exit code 1 on the first
    mismatch (with the vector, op index, expected vs actual)."""

    def __init__(self, port, token):
        self.port = port
        self.token = token
        self.client = None

    def connect(self, token=None):
        self.client = McpClient("127.0.0.1", self.port,
                                token=token or self.token).connect()
        self.client.initialize()

    def run_op(self, op, vars_, last):
        """Execute one op; returns (result, new_last)."""
        name = op["op"]
        koid = resolve(op["koid"], vars_) if "koid" in op else None
        if name == "connect":
            self.connect(op.get("token"))
            return {}, last
        if name == "close":
            self.client.close()
            return {}, last
        if name == "health":
            return self.client.health(), last
        if name == "metrics":
            return self.client.metrics(), last
        if name == "remember":
            r = self.client.remember(op["type"], op.get("properties", {}))
            return r, r["koid"]
        if name == "update":
            r = self.client.remember(op["type"], op.get("properties", {}),
                                     koid=koid or last)
            return r, r["koid"]
        if name == "get":
            return self.client.get(koid or last), last
        if name == "delete":
            r = self.client.forget(koid or last, mode="tombstone")
            return r, r["koid"]
        if name == "query":
            if op.get("stream"):
                return {"chunks": list(self.client.aikoql_stream(op["query"]))}, last
            return self.client.aikoql(op["query"]), last
        if name == "relate":
            r = self.client.relate(resolve(op["from"], vars_),
                                   resolve(op["to"], vars_), op["rel_type"])
            return r, r["koid"]
        if name == "traverse":
            return self.client.traverse(koid, op.get("rel_type"),
                                        op.get("depth", 1)), last
        if name == "find_similar":
            return self.client.find_similar(
                text=op.get("text"),
                wait_for_freshness_ms=op.get("wait_for_freshness_ms")), last
        if name == "begin":
            tx = self.client.begin()
            vars_[op["as"]] = tx  # txn handles and koids share the var map
            return {"txn_id": tx.txn_id}, last
        if name == "execute":
            tx = resolve(op["txn"], vars_)
            tx.execute(op["action"], type_name=op.get("type"),
                       properties=op.get("properties"))
            return {}, last
        if name == "commit":
            return resolve(op["txn"], vars_).commit(), last
        if name == "rollback":
            return resolve(op["txn"], vars_).rollback(), last
        if name == "explain":
            return self.client.explain(koid), last
        if name == "trace":
            return self.client.trace(koid), last
        if name == "discover_schema":
            return self.client.discover_schema(), last
        raise AssertionError(f"op {name!r} has no adapter arm")

    def capture(self, op, result, vars_):
        """`as` saves the result koid (a commit's first write result) for
        later $refs."""
        if "as" not in op or not isinstance(result, dict):
            return
        if "koid" in result:
            vars_[op["as"]] = result["koid"]
        elif result.get("results"):
            vars_[op["as"]] = result["results"][0]["koid"]

    def check(self, op, result, vars_):
        for path, want in (op.get("assert") or {}).items():
            try:
                got = dot_get(result, path)
            except (KeyError, IndexError, TypeError) as e:
                raise AssertionError(f"assert {path}: {e}") from e
            want = resolve(want, vars_)
            if got != want:
                raise AssertionError(
                    f"assert {path}: expected {want!r}, got {got!r}")
        aa = op.get("assert_any")
        if aa:
            items = dot_get(result, aa["path"])
            match = aa["match"]
            if isinstance(match, dict):
                ok = any(all(dot_get(e, p) == resolve(v, vars_)
                             for p, v in match.items()) for e in items)
            else:
                ok = any(e == match for e in items)
            if not ok:
                raise AssertionError(
                    f"assert_any {aa['path']}: no element matches {match!r}")

    def run_vector(self, ops):
        self.connect()
        vars_, last = {}, None
        try:
            for i, op in enumerate(ops):
                expect = op.get("expect_error")
                try:
                    result, last = self.run_op(op, vars_, last)
                except McpError as e:
                    if expect == map_code(e):
                        continue
                    raise AssertionError(
                        f"op {i} {op['op']}: expected error {expect}, "
                        f"got {map_code(e)}: {e.message}")
                if expect:
                    raise AssertionError(
                        f"op {i} {op['op']}: expected error {expect}, "
                        f"none raised")
                self.capture(op, result, vars_)
                self.check(op, result, vars_)
        finally:
            self.client.close()


def load_vectors(vectors_dir, protocol_dir):
    """All vector files: the frozen protocol vectors first, then the
    conformance root (canonical.json) and the category dirs."""
    paths = sorted(os.path.join(protocol_dir, p)
                   for p in _walk_json(protocol_dir))
    paths += sorted(os.path.join(vectors_dir, p)
                    for p in _walk_json(vectors_dir))
    for path in paths:
        with open(path, encoding="utf-8") as fh:
            vec = json.load(fh)
        yield path, vec["name"], vec["operations"]


def _walk_json(root):
    found = []
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if f.endswith(".json"):
                found.append(os.path.relpath(os.path.join(dirpath, f), root))
    return found


def spawn_server(bin_path, token):
    """The conftest pattern: a probed free port, a non-existent db path the
    server auto-creates as aikoql-v2, readiness by dial, stderr surfaced."""
    # The dispatcher hands bash-form paths (forward slashes, relative);
    # CreateProcess rejects both in lpApplicationName (WinError 2), so
    # normalize once here instead of in every caller.
    bin_path = os.path.abspath(bin_path)
    fd, db = tempfile.mkstemp(prefix="conformance-", suffix=".redb")
    os.close(fd)
    os.unlink(db)
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    proc = subprocess.Popen(
        [bin_path, "serve", db, "--listen", f"127.0.0.1:{port}",
         "--tcp-token", token],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    deadline = time.time() + 15
    while time.time() < deadline:
        if proc.poll() is not None:
            raise AssertionError(
                f"server exited early: "
                f"{proc.stderr.read().decode(errors='replace')}")
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
            break
        except OSError:
            time.sleep(0.05)
    # The server logs everything to stderr and one server lives for the
    # whole run: drain the pipe or a full buffer wedges the server mid-log.
    threading.Thread(target=_drain_stderr, args=(proc.stderr,),
                     daemon=True).start()
    return proc, port, db


def _drain_stderr(pipe):
    while True:
        if not pipe.readline():
            return


def teardown_server(proc, db):
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    shutil.rmtree(db, ignore_errors=True)
    try:
        os.remove(db)
    except OSError:
        pass
    try:
        os.remove(db + ".audit.log")
    except OSError:
        pass


def main(argv):
    args = {}
    i = 1
    while i < len(argv):
        key = argv[i]
        if not key.startswith("--"):
            raise SystemExit(f"unknown arg {key!r}")
        args[key[2:]] = argv[i + 1]
        i += 2
    bin_path = args["bin"]
    vectors_dir = args["vectors"]
    protocol_dir = args["protocol"]
    token = args.get("token", "conformance")
    proc, port, db = spawn_server(bin_path, token + "::admin")
    runner = Runner(port, token)
    vectors = total_ops = 0
    try:
        for path, name, ops in load_vectors(vectors_dir, protocol_dir):
            try:
                runner.run_vector(ops)
            except Exception as e:
                raise AssertionError(f"{path}: {e}") from e
            vectors += 1
            total_ops += len(ops)
            print(f"  ok {name} ({len(ops)} ops)", flush=True)
    finally:
        teardown_server(proc, db)
    print(f"sdk-conformance (python): {vectors} vectors, "
          f"{total_ops} ops — all passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
