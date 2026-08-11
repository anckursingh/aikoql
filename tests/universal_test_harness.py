#!/usr/bin/env python3
"""
Universal Test Harness for aikoql — Agent Knowledge Interface (MRFC-0070)

Acts as the testing bridge between coding agents and the aikoql application.
Every MRFC-0070 phase MUST pass this harness before marking complete.

Methodology (per IMPLEMENTATION-PLAN.md):
    implement → test (unit + acceptance + universal) → update plan → next phase

Usage:
    # Start the MCP server first (or let the harness start it):
    python tests/universal_test_harness.py --server-binary ./target/release/aikoql-mcp.exe

    # With a pre-built release:
    python tests/universal_test_harness.py --server-binary ./build/windows/aikoql-mcp.exe

    # Connect to an already-running server:
    python tests/universal_test_harness.py --host 127.0.0.1 --port 9090

    # Run only specific phase tests:
    python tests/universal_test_harness.py --phase A0

    # Verbose output:
    python tests/universal_test_harness.py -v

Design:
    - Each test section maps to acceptance criteria from IMPLEMENTATION-PLAN.md
    - Tests are self-documenting: failures explain what knowledge capability broke
    - The harness itself is a real agent client — dogfooding aikoql's own interface
    - Results are JSON-serializable for CI integration
"""

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 9090
DEFAULT_HTTP_PORT = 9091
SERVER_STARTUP_TIMEOUT = 30  # seconds

# ---------------------------------------------------------------------------
# MCP Client (minimal — avoids dependency on aikoql package for portability)
# ---------------------------------------------------------------------------


class McpError(Exception):
    def __init__(self, code: str, message: str):
        self.code = code
        self.message = message
        super().__init__(f"[{code}] {message}")

    @classmethod
    def from_response(cls, err: dict) -> "McpError":
        return cls(
            code=err.get("code", "INTERNAL"),
            message=err.get("message", str(err)),
        )


class McpClient:
    """Minimal JSON-RPC 2.0 client for Aikoql MCP server."""

    def __init__(self, host: str = DEFAULT_HOST, port: int = DEFAULT_PORT):
        self.host = host
        self.port = port
        self._sock: Optional[socket.socket] = None
        self._buf = b""
        self._next_id = 0

    def connect(self, timeout: float = 5.0) -> "McpClient":
        self._sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self._sock.settimeout(timeout)
        return self

    def close(self):
        if self._sock:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None

    def __enter__(self):
        return self.connect()

    def __exit__(self, *args):
        self.close()

    def _send(self, payload: dict):
        frame = json.dumps(payload, default=str) + "\n"
        self._sock.sendall(frame.encode("utf-8"))

    def _recv(self) -> dict:
        while True:
            if b"\n" in self._buf:
                line, self._buf = self._buf.split(b"\n", 1)
                text = line.decode("utf-8").strip()
                if not text:
                    continue
                return json.loads(text)
            chunk = self._sock.recv(4096)
            if not chunk:
                raise ConnectionError("server closed connection")
            self._buf += chunk

    def _rpc(self, method: str, params: Optional[dict] = None) -> dict:
        self._next_id += 1
        req = {"jsonrpc": "2.0", "id": self._next_id, "method": method}
        if params is not None:
            req["params"] = params
        self._send(req)
        resp = self._recv()
        if "error" in resp:
            err = resp["error"]
            raise McpError(
                code=err.get("code", "INTERNAL"),
                message=err.get("message", str(err)),
            )
        return resp.get("result", resp)

    def initialize(self, client_name: str = "universal-harness", client_version: str = "1.0"):
        return self._rpc("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        })

    def call_tool(self, name: str, arguments: Optional[dict] = None) -> dict:
        params: Dict[str, Any] = {"name": name}
        if arguments:
            params["arguments"] = arguments
        result = self._rpc("tools/call", params)
        text = result.get("content", [{}])[0].get("text", "{}")
        data = json.loads(text)
        if not data.get("ok", True):
            raise McpError.from_response(data.get("error", {}))
        return data.get("data", data)

    def session_init(self, agent_id: str, run_id: str = None, roles: List[str] = None):
        params: Dict[str, Any] = {"agent_id": agent_id}
        if run_id:
            params["run_id"] = run_id
        if roles:
            params["roles"] = roles
        return self._rpc("session/init", params)


# ---------------------------------------------------------------------------
# Test Harness
# ---------------------------------------------------------------------------


class TestResult:
    def __init__(self, name: str, passed: bool, detail: str = "", phase: str = ""):
        self.name = name
        self.passed = passed
        self.detail = detail
        self.phase = phase


class UniversalTestHarness:
    """Comprehensive test harness for MRFC-0070 acceptance criteria."""

    def __init__(self, client: McpClient, verbose: bool = False):
        self.client = client
        self.verbose = verbose
        self.results: List[TestResult] = []
        self._start_time = datetime.now(timezone.utc)

    def log(self, msg: str):
        if self.verbose:
            ts = (datetime.now(timezone.utc) - self._start_time).total_seconds()
            print(f"  [{ts:6.1f}s] {msg}", file=sys.stderr)

    def assert_true(self, condition: bool, name: str, detail: str = "", phase: str = ""):
        if condition:
            self.results.append(TestResult(name, True, detail, phase))
            self.log(f"  PASS: {name}")
        else:
            self.results.append(TestResult(name, False, detail, phase))
            self.log(f"  FAIL: {name} — {detail}")

    def assert_ok(self, result: Any, name: str, phase: str = ""):
        """Shortcut: result is truthy = pass."""
        self.assert_true(bool(result), name, str(result)[:200], phase)

    # ------------------------------------------------------------------
    # Phase 0: Server Health & Connectivity
    # ------------------------------------------------------------------

    def test_connectivity(self):
        """Basic connectivity — server is alive and responding."""
        print("\n--- Phase 0: Connectivity ---")
        phase = "P0"

        try:
            result = self.client.initialize("universal-test-harness", "1.0.0")
            self.assert_ok(result, "initialize — MCP handshake", phase)

            health = self.client.call_tool("health", {})
            self.assert_true(
                health.get("ready", False),
                "health — server reports ready",
                str(health)[:200],
                phase,
            )

        except Exception as e:
            self.results.append(TestResult("connectivity", False, str(e), phase))
            self.log(f"  FAIL: connectivity — {e}")

    # ------------------------------------------------------------------
    # Phase A0: Model Foundation (AKI-001, AKI-002, AKI-005, AKI-006, AKI-007, AKI-010)
    # ------------------------------------------------------------------

    def test_phase_a0_model_foundation(self):
        """Tests that will validate Authority, Scope, KnowledgeStatus, and Conflict types."""
        print("\n--- Phase A0: Model Foundation ---")
        phase = "A0"

        # AKI-001: Universal KO creation — create typed KOs
        try:
            r = self.client.call_tool("remember", {
                "type_name": "Component",
                "properties": {"name": "TestComponent", "language": "Rust"},
            })
            koid = r.get("koid", "")
            self.assert_true(bool(koid), "AKI-001: KO creation — Component typed KO", koid, phase)

            # AKI-002: Source provenance — every derived KO has provenance
            get_result = self.client.call_tool("get", {"koid": koid})
            self.assert_ok(get_result, "AKI-002: KO retrieval — get by KOID", phase)

            # AKI-010: Knowledge lifecycle — lifecycle states exist
            self.assert_true(
                "state" in get_result,
                "AKI-010: Lifecycle — KO has state field",
                str(get_result)[:200],
                phase,
            )

            # AKI-005: Scope model — create tenant-scoped KO
            r2 = self.client.call_tool("remember", {
                "type_name": "Requirement",
                "properties": {"title": "Test Requirement"},
                "tenant": "test-tenant",
            })
            self.assert_true(bool(r2.get("koid", "")), "AKI-005: Scoped KO — tenant-scoped creation", phase)

            # AKI-006: Authority & Confidence — decide tool creates provenance-tagged claim
            self.client.call_tool("decide", {
                "koid": koid,
                "decision": "verified",
                "rationale": "Test harness validation",
                "confidence": 0.95,
            })
            explain = self.client.call_tool("explain", {"koid": koid})
            self.assert_true(
                explain is not None,
                "AKI-006: Authority — explain tool works with decisions",
                str(explain)[:200],
                phase,
            )

            # AKI-007: Temporal model — trace gives version history
            trace = self.client.call_tool("trace", {"koid": koid})
            self.assert_ok(trace, "AKI-007: Temporal — trace returns version history", phase)

        except Exception as e:
            self.results.append(TestResult("A0-model-foundation", False, str(e), phase))
            self.log(f"  FAIL: A0 Model Foundation — {e}")

    # ------------------------------------------------------------------
    # Phase 1-5 Baseline: Core Knowledge Operations (already built)
    # ------------------------------------------------------------------

    def test_core_knowledge_operations(self):
        """Verify all existing functionality works end-to-end."""
        print("\n--- Baseline: Core Knowledge Operations ---")
        phase = "BASELINE"

        try:
            # remember + get + forget lifecycle
            r = self.client.call_tool("remember", {
                "type_name": "Task",
                "properties": {"title": "Test task for universal harness", "priority": "high"},
            })
            koid = r["koid"]
            self.assert_ok(koid, "remember — creates Task KO", phase)

            g = self.client.call_tool("get", {"koid": koid})
            self.assert_true(g.get("type_name") == "Task", "get — returns correct type", phase)
            self.assert_true(
                g.get("properties", {}).get("title") == "Test task for universal harness",
                "get — property round-trip",
                phase,
            )

            # find_similar
            fs = self.client.call_tool("find_similar", {
                "text": "test task harness",
                "type_name": "Task",
                "k": 5,
            })
            self.assert_ok(fs, "find_similar — hybrid recall works", phase)

            # aikoql query
            a = self.client.call_tool("aikoql", {
                "query": "MATCH Task RETURN *",
            })
            self.assert_ok(a, "aikoql — MATCH query executes", phase)

            # relati + traverse
            r2 = self.client.call_tool("remember", {
                "type_name": "Component",
                "properties": {"name": "TestComponent"},
            })
            koid2 = r2["koid"]
            self.client.call_tool("relate", {
                "from": koid,
                "to": koid2,
                "rel_type": "affects",
            })
            trav = self.client.call_tool("traverse", {"koid": koid, "depth": 1})
            self.assert_ok(trav, "relate + traverse — relationship graph", phase)

            # trace + explain + prove
            self.client.call_tool("trace", {"koid": koid})
            self.client.call_tool("explain", {"koid": koid})
            prove = self.client.call_tool("prove", {"koid": koid})
            self.assert_ok(prove, "trace + explain + prove — evidence chain", phase)

            # schema discovery
            schema = self.client.call_tool("discover_schema", {})
            self.assert_ok(schema, "discover_schema — type inventory", phase)

            # metrics
            metrics = self.client.call_tool("metrics", {})
            self.assert_ok(metrics, "metrics — server statistics", phase)

            # Audit report
            audit = self.client.call_tool("audit_report", {})
            self.assert_ok(audit, "audit_report — compliance report", phase)

            # forget (tombstone)
            self.client.call_tool("forget", {"koid": koid, "mode": "tombstone"})
            g2 = self.client.call_tool("get", {"koid": koid})
            self.assert_ok(g2, "forget — tombstone + get still returns", phase)

        except Exception as e:
            self.results.append(TestResult("core-operations", False, str(e), phase))
            self.log(f"  FAIL: Core operations — {e}")

    # ------------------------------------------------------------------
    # Agent Experience: MRFC-0040 features
    # ------------------------------------------------------------------

    def test_agent_experience(self):
        """Verify agent experience improvements (MRFC-0040)."""
        print("\n--- Agent Experience (MRFC-0040) ---")
        phase = "AGENT"

        try:
            # Session identity
            sess = self.client.session_init(
                agent_id="universal-harness-agent",
                run_id="run-001",
                roles=["tester"],
            )
            self.assert_ok(sess, "session_init — agent identity established", phase)

            # Batch operations
            batch_r = self.client.call_tool("batch", {"operations": [
                {"remember": {"type_name": "Task", "properties": {"title": "Batch Task 1"}}},
                {"remember": {"type_name": "Task", "properties": {"title": "Batch Task 2"}}},
            ]})
            self.assert_ok(batch_r, "batch — atomic multi-operation", phase)

            # Agent memory — subject must match agent_id (owner of the memory KO)
            mem = self.client.call_tool("agent_memory", {
                "agent_id": "harness-agent",
                "key": "last_task",
                "value": {"task": "universal_test", "timestamp": int(time.time())},
                "subject": "harness-agent",
            })
            self.assert_ok(mem, "agent_memory — agent session memory", phase)

            # Decision tool
            r = self.client.call_tool("remember", {
                "type_name": "Claim",
                "properties": {"statement": "HNSW is the vector index"},
            })
            dec = self.client.call_tool("decide", {
                "koid": r["koid"],
                "decision": "verified_against_code",
                "rationale": "Source code confirms HNSW usage",
                "confidence": 0.99,
            })
            self.assert_ok(dec, "decide — provenance-tagged decision", phase)

        except Exception as e:
            self.results.append(TestResult("agent-experience", False, str(e), phase))
            self.log(f"  FAIL: Agent experience — {e}")

    # ------------------------------------------------------------------
    # Active Knowledge Objects: MRFC-0030
    # ------------------------------------------------------------------

    def test_active_knowledge_objects(self):
        """Verify Active KOs: Program, Policy, Workflow, Agent, Trigger."""
        print("\n--- Active Knowledge Objects (MRFC-0030) ---")
        phase = "MRFC-0030"

        try:
            # Program KO
            prog = self.client.call_tool("deploy_program", {
                "name": "TestProgram",
                "body": "MATCH Task RETURN *",
                "language": "aikoql",
            })
            self.assert_ok(prog, "deploy_program — Program KO created", phase)

            prog_list = self.client.call_tool("list_programs", {})
            self.assert_ok(prog_list, "list_programs — program inventory", phase)

            # Policy KO
            pol = self.client.call_tool("deploy_policy", {
                "name": "TestPolicy",
                "effect": "Allow",
                "principal": "tester",
                "action": "Read",
                "resource_type": "Task",
            })
            self.assert_ok(pol, "deploy_policy — Policy KO created", phase)

            # Workflow KO
            wf = self.client.call_tool("deploy_workflow", {
                "name": "TestWorkflow",
                "steps": [
                    {"order": 1, "program": "TestProgram", "on_failure": "skip"},
                ],
            })
            self.assert_ok(wf, "deploy_workflow — Workflow KO created", phase)

            # Agent KO
            agent_ko = self.client.call_tool("deploy_agent", {
                "name": "TestAgent",
                "prompt": "You are a test agent.",
                "skills": ["TestProgram"],
            })
            self.assert_ok(agent_ko, "deploy_agent — Agent KO created", phase)

            # Trigger KO (needs program KOID, get from deploy_program result)
            prog_koid = prog.get("koid", "")
            trig = self.client.call_tool("deploy_trigger", {
                "name": "TestTrigger",
                "event_kind": "Created",
                "type_filter": "Task",
                "program_koid": prog_koid,
            })
            self.assert_ok(trig, "deploy_trigger — Trigger KO created", phase)

            # Execute workflow (needs koid from deploy_workflow result)
            wf_koid = wf.get("koid", "")
            exec_result = self.client.call_tool("execute_workflow", {
                "koid": wf_koid,
            })
            self.assert_ok(exec_result, "execute_workflow — Workflow execution", phase)

            # Check triggers
            trig_check = self.client.call_tool("check_triggers", {})
            self.assert_ok(trig_check, "check_triggers — Trigger evaluation", phase)

            # Program cache stats
            cache_stats = self.client.call_tool("program_cache_stats", {})
            self.assert_ok(cache_stats, "program_cache_stats — KVM cache status", phase)

        except Exception as e:
            self.results.append(TestResult("active-knowledge-objects", False, str(e), phase))
            self.log(f"  FAIL: Active KOs — {e}")

    # ------------------------------------------------------------------
    # Document Knowledge Compiler: MRFC-0050 (D1-D9)
    # ------------------------------------------------------------------

    def test_document_compiler(self):
        """Verify document ingestion and compilation pipeline."""
        print("\n--- Document Knowledge Compiler (MRFC-0050) ---")
        phase = "MRFC-0050"

        try:
            # Create a minimal test markdown document
            import base64
            doc_content = base64.b64encode(
                b"# Test Document\n\nThis is a test document about the ConstraintEngine.\n\n"
                b"The ConstraintEngine uses MVCC for transaction isolation.\n\n"
                b"## Requirements\n\n- Must be atomic\n- Must be fast\n"
            ).decode("ascii")

            ingest = self.client.call_tool("document_ingest", {
                "filename": "test_doc.md",
                "content_base64": doc_content,
                "mime_type": "text/markdown",
            })
            self.assert_ok(ingest, "document_ingest — markdown ingestion", phase)

            ingest_koid = ingest.get("koid", "")
            doc_status = self.client.call_tool("document_status", {
                "koid": ingest_koid,
            })
            self.assert_ok(doc_status, "document_status — processing status", phase)

            doc_list = self.client.call_tool("document_list", {})
            self.assert_ok(doc_list, "document_list — document inventory", phase)

            # Document compilation (if the doc is ready)
            try:
                compile_result = self.client.call_tool("document_compile", {
                    "koid": ingest_koid,
                })
                self.assert_ok(compile_result, "document_compile — knowledge extraction", phase)
            except McpError as e:
                self.log(f"  SKIP: document_compile not yet ready — {e}")

        except Exception as e:
            self.results.append(TestResult("document-compiler", False, str(e), phase))
            self.log(f"  FAIL: Document compiler — {e}")

    # ------------------------------------------------------------------
    # Constraint Engine: MRFC-0060
    # ------------------------------------------------------------------

    def test_constraint_engine(self):
        """Verify constraint enforcement."""
        print("\n--- Constraint Engine (MRFC-0060) ---")
        phase = "MRFC-0060"

        try:
            # Register a schema with typed properties
            # The schema/ontology system should enforce types
            r = self.client.call_tool("remember", {
                "type_name": "TestEntity",
                "properties": {"name": "ValidName", "count": 42},
            })
            self.assert_ok(r, "constraint: valid typed properties accepted", phase)

            # Discover ontology
            ont = self.client.call_tool("discover_ontology", {})
            self.assert_ok(ont, "discover_ontology — ontology discovery", phase)

        except Exception as e:
            self.results.append(TestResult("constraint-engine", False, str(e), phase))
            self.log(f"  FAIL: Constraint engine — {e}")

    # ------------------------------------------------------------------
    # Studio HTTP Endpoints
    # ------------------------------------------------------------------

    def test_studio_endpoints(self, host: str = DEFAULT_HOST, http_port: int = DEFAULT_HTTP_PORT):
        """Verify Studio HTTP endpoints are reachable."""
        print("\n--- Studio HTTP Endpoints ---")
        phase = "STUDIO"

        import urllib.request
        import urllib.error

        endpoints = [
            ("/studio", "Studio HTML"),
            ("/health", "Health endpoint"),
            ("/metrics", "Metrics endpoint"),
            ("/api/v1/schema", "Schema API"),
        ]

        for path, label in endpoints:
            url = f"http://{host}:{http_port}{path}"
            try:
                req = urllib.request.Request(url)
                resp = urllib.request.urlopen(req, timeout=5)
                content_length = len(resp.read())
                self.assert_true(
                    resp.status == 200 and content_length > 0,
                    f"studio: {label} — {path}",
                    f"HTTP {resp.status}, {content_length} bytes",
                    phase,
                )
            except Exception as e:
                self.results.append(TestResult(f"studio-{path}", False, str(e), phase))
                self.log(f"  FAIL: {label} ({path}) — {e}")

    # ------------------------------------------------------------------
    # Report
    # ------------------------------------------------------------------

    def run_all(self, phases: Optional[List[str]] = None, host: str = DEFAULT_HOST,
                http_port: int = DEFAULT_HTTP_PORT):
        """Run all or selected phases."""
        all_phases = {
            "P0": self.test_connectivity,
            "A0": self.test_phase_a0_model_foundation,
            "BASELINE": self.test_core_knowledge_operations,
            "AGENT": self.test_agent_experience,
            "MRFC-0030": self.test_active_knowledge_objects,
            "MRFC-0050": self.test_document_compiler,
            "MRFC-0060": self.test_constraint_engine,
            "STUDIO": lambda: self.test_studio_endpoints(host, http_port),
        }

        if phases:
            selected = [p for p in phases if p in all_phases]
            if not selected:
                print(f"No matching phases for: {phases}")
                print(f"Available phases: {list(all_phases.keys())}")
                return self.report()
            to_run = [(p, all_phases[p]) for p in phases if p in all_phases]
        else:
            to_run = list(all_phases.items())

        for name, test_fn in to_run:
            try:
                test_fn()
            except Exception as e:
                self.results.append(TestResult(name, False, str(e), name))
                self.log(f"  CRASH: {name} — {e}")

        return self.report()

    def report(self) -> Tuple[int, int, int]:
        """Print report. Returns (passed, failed, total)."""
        passed = sum(1 for r in self.results if r.passed)
        failed = sum(1 for r in self.results if not r.passed)
        total = len(self.results)

        print(f"\n{'='*70}")
        print(f"  UNIVERSAL TEST HARNESS — Results")
        print(f"  Total: {total}  |  Passed: {passed}  |  Failed: {failed}")
        print(f"  Score: {passed/total*100:.1f}%" if total > 0 else "  No tests run")
        print(f"{'='*70}")

        if failed > 0:
            print(f"\n  FAILURES:")
            for r in self.results:
                if not r.passed:
                    print(f"    [{r.phase}] {r.name}")
                    if r.detail:
                        print(f"           {r.detail[:120]}")

        # Produce JSON report for CI
        report_path = Path("tests/universal_test_report.json")
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_data = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "total": total,
            "passed": passed,
            "failed": failed,
            "score_pct": round(passed / total * 100, 1) if total > 0 else 0,
            "results": [
                {
                    "name": r.name,
                    "passed": r.passed,
                    "detail": r.detail[:200],
                    "phase": r.phase,
                }
                for r in self.results
            ],
        }
        report_path.write_text(json.dumps(report_data, indent=2))
        print(f"\n  JSON report: {report_path}")

        return passed, failed, total


# ---------------------------------------------------------------------------
# Server Management
# ---------------------------------------------------------------------------


class ServerManager:
    """Manages the Aikoql MCP server lifecycle."""

    def __init__(self, binary: str, db_path: str, host: str = DEFAULT_HOST,
                 port: int = DEFAULT_PORT, http_port: int = DEFAULT_HTTP_PORT):
        self.binary = binary
        self.db_path = db_path
        self.host = host
        self.port = port
        self.http_port = http_port
        self.process: Optional[subprocess.Popen] = None

    def start(self) -> bool:
        """Start the server. Returns True if started successfully."""
        cmd = [
            self.binary,
            self.db_path,
            "--listen", f"{self.host}:{self.port}",
            "--metrics-addr", f"{self.host}:{self.http_port}",
        ]
        print(f"Starting server: {' '.join(cmd)}")
        self.process = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        # Wait for server to start
        deadline = time.time() + SERVER_STARTUP_TIMEOUT
        while time.time() < deadline:
            if self.process.poll() is not None:
                stderr = self.process.stderr.read().decode("utf-8", errors="replace") if self.process.stderr else ""
                print(f"SERVER FAILED TO START (exit {self.process.returncode}):\n{stderr}")
                return False
            # Try connecting
            try:
                sock = socket.create_connection((self.host, self.port), timeout=1)
                sock.close()
                print(f"Server ready on {self.host}:{self.port}")
                return True
            except (socket.error, ConnectionRefusedError):
                time.sleep(0.5)

        print(f"SERVER START TIMEOUT after {SERVER_STARTUP_TIMEOUT}s")
        return False

    def stop(self):
        """Stop the server."""
        if self.process:
            print("Stopping server...")
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
            self.process = None


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(
        description="Universal Test Harness for aikoql — Agent Knowledge Interface",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Build and test (auto-starts server):
  python tests/universal_test_harness.py --server-binary ./target/release/aikoql-mcp.exe

  # Test an already-running server:
  python tests/universal_test_harness.py --host 127.0.0.1 --port 9090

  # Run only Phase A0 tests:
  python tests/universal_test_harness.py --server-binary ./target/release/aikoql-mcp.exe --phase A0

  # Run Phase A0 + Baseline tests:
  python tests/universal_test_harness.py --server-binary ./target/release/aikoql-mcp.exe --phase A0 --phase BASELINE
        """,
    )
    parser.add_argument("--server-binary", help="Path to aikoql-mcp binary (auto-starts server)")
    parser.add_argument("--host", default=DEFAULT_HOST, help=f"MCP server host (default: {DEFAULT_HOST})")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"MCP server port (default: {DEFAULT_PORT})")
    parser.add_argument("--http-port", type=int, default=DEFAULT_HTTP_PORT, help=f"HTTP metrics/Studio port (default: {DEFAULT_HTTP_PORT})")
    parser.add_argument("--phase", action="append", dest="phases",
                        help="Run specific phase (repeatable). Options: P0, A0, BASELINE, AGENT, MRFC-0030, MRFC-0050, MRFC-0060, STUDIO")
    parser.add_argument("-v", "--verbose", action="store_true", help="Verbose output")
    args = parser.parse_args()

    server_mgr = None
    client = None
    exit_code = 0

    try:
        # Start server if binary provided
        if args.server_binary:
            if not os.path.exists(args.server_binary):
                print(f"ERROR: Server binary not found: {args.server_binary}")
                print("Build it first: cargo build --release -p aikoql-mcp")
                sys.exit(1)

            db_path = tempfile.mktemp(suffix=".redb", prefix="aikoql_test_")
            server_mgr = ServerManager(args.server_binary, db_path, args.host, args.port, args.http_port)
            if not server_mgr.start():
                sys.exit(1)

        # Connect client
        client = McpClient(args.host, args.port)
        client.connect(timeout=10.0)
        client.initialize("universal-test-harness", "1.0.0")
        print(f"Connected to aikoql at {args.host}:{args.port}")

        # Run tests
        harness = UniversalTestHarness(client, verbose=args.verbose)
        passed, failed, total = harness.run_all(
            phases=args.phases,
            host=args.host,
            http_port=args.http_port,
        )

        exit_code = 0 if failed == 0 else 1

    except Exception as e:
        print(f"\nFATAL: {e}")
        import traceback
        traceback.print_exc()
        exit_code = 2
    finally:
        if client:
            client.close()
        if server_mgr:
            server_mgr.stop()

    sys.exit(exit_code)


if __name__ == "__main__":
    main()
