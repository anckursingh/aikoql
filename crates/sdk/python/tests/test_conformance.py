"""D-11: the shared conformance runner (§7, §23).

`scripts/sdk-conformance.sh --language python` executes the §23 canonical
workload and the §7 category vectors from tests/sdk-conformance/ (plus the
frozen protocol/test-vectors/) against a real server through this SDK, with
the same expected results every language must produce. The runner is the
single entry point, so this test pins only the CLI contract.
"""

import os
import subprocess

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))


def test_sdk_conformance_python():
    # bash mangles Windows backslash paths — hand it forward slashes.
    script = (ROOT + os.sep + "scripts" + os.sep
              + "sdk-conformance.sh").replace("\\", "/")
    proc = subprocess.run(
        ["bash", script, "--language", "python"],
        capture_output=True, text=True, timeout=300)
    assert proc.returncode == 0, (
        f"sdk-conformance --language python failed ({proc.returncode}):\n"
        f"{proc.stdout}\n{proc.stderr}")
