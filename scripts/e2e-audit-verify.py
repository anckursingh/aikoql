#!/usr/bin/env python3
"""AG-30: an evidence pack's audit signature, verified outside aikoql.

Starts a server with a fresh Ed25519 key (AIKOQL_AUDIT_SIGNING_KEY), writes
a few objects, asks for the evidence pack, and verifies `audit_signature`
with the Python `cryptography` package — an implementation that shares no
code with aikoql — from the public key alone. A tampered chain must fail.

  pip install cryptography
  python scripts/e2e-audit-verify.py [path/to/aikoql-mcp]
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/aikoql-mcp"
    root = tempfile.mkdtemp(prefix="e2e-audit-verify-")
    try:
        key = os.path.join(root, "audit.key")
        with open(key, "w") as f:
            f.write(os.urandom(32).hex() + "\n")
        os.chmod(key, 0o600)
        env = {"AIKOQL_AUDIT_SIGNING_KEY": key, "HOME": root, "PATH": "/usr/bin:/bin"}
        proc = subprocess.Popen([binary, "serve", os.path.join(root, "db")], env=env,
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, text=True)
        calls = [{"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": {"protocolVersion": "2025-06-18", "capabilities": {}}}]
        for i in range(3):
            calls.append({"jsonrpc": "2.0", "id": 10 + i, "method": "tools/call", "params": {
                "name": "remember", "arguments": {"type_name": "note", "properties": {"n": i}}}})
        calls.append({"jsonrpc": "2.0", "id": 99, "method": "tools/call",
                      "params": {"name": "evidence_pack", "arguments": {"framework": "gdpr"}}})
        for c in calls:
            proc.stdin.write(json.dumps(c) + "\n")
        proc.stdin.flush()
        pack = None
        for line in proc.stdout:
            frame = json.loads(line)
            if frame.get("id") == 99:
                pack = json.loads(frame["result"]["content"][0]["text"])
                break
        proc.kill()
        sig = pack["audit_signature"]
        assert sig["algorithm"] == "ed25519" and sig["signed"] == "audit_chain", sig
        chain = bytes.fromhex(pack["audit_chain"])
        public = Ed25519PublicKey.from_public_bytes(bytes.fromhex(sig["public_key"]))
        public.verify(bytes.fromhex(sig["signature"]), chain)  # raises if invalid
        print(f"verified: journal seq {pack['journal_seq']}, chain {pack['audit_chain'][:16]}…, "
              f"public key {sig['public_key'][:16]}…")
        tampered = bytes([chain[0] ^ 1]) + chain[1:]
        try:
            public.verify(bytes.fromhex(sig["signature"]), tampered)
            sys.exit("e2e-audit-verify: FAIL a tampered chain verified")
        except InvalidSignature:
            print("a tampered chain is rejected")
        print("e2e-audit-verify: PASS — the audit chain's Ed25519 signature checks out with "
              "an independent implementation and the public key alone")
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
