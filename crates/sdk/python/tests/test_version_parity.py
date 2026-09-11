"""sdk001 — the version-parity contract (P3-M9 §72): the Python package's
version must equal the workspace version. The workspace version is parsed
from the root Cargo.toml, never hardcoded, so any future workspace bump
turns this test RED by itself. The package version must exist as
`aikoql.__version__` (exported by the PyO3 module from CARGO_PKG_VERSION —
single source of truth) and match the pyproject metadata.

Requires: the package importable (maturin develop or PYTHONPATH).
Run: pytest tests/test_version_parity.py -v
"""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "python"))


def workspace_version() -> str:
    root = Path(__file__).parent.parent.parent.parent.parent / "Cargo.toml"
    text = root.read_text(encoding="utf-8")
    m = re.search(r'\[workspace\.package\]\s*version = "([^"]+)"', text)
    assert m, "workspace.package version not found in root Cargo.toml"
    return m.group(1)


def test_package_version_matches_workspace():
    import aikoql
    from importlib.metadata import version as dist_version

    want = workspace_version()
    # 1. The attribute the SDK itself advertises.
    assert getattr(aikoql, "__version__", None) == want, (
        f"aikoql.__version__ = {getattr(aikoql, '__version__', None)!r}, "
        f"workspace = {want!r}"
    )
    # 2. The installed distribution metadata (pyproject/maturin).
    assert dist_version("aikoql") == want, (
        f"installed aikoql distribution = {dist_version('aikoql')!r}, "
        f"workspace = {want!r}"
    )
