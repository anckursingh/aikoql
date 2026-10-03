"""D-16 §10: the Python fuzz estate's own pin (the F-04 pattern — a
removed target is a detected coverage loss, never silent).

The estate lives in test_fuzz_estate.py: the §12 protocol state machine
(DISCONNECTED→CONNECTED→INITIALIZED→TRANSACTION→STREAMING→CLOSED) plus
the L1/L3 hypothesis property tests.
"""

from pathlib import Path

_ESTATE = Path(__file__).with_name("test_fuzz_estate.py")

# The §12 machine and the four property tests, by name.
_PINS = [
    "class ProtocolMachine",
    "def test_parse_version_property",
    "def test_mcp_error_property",
    "def test_envelope_property",
    "def test_frame_cap_property",
]


def test_fuzz_estate_pin():
    src = _ESTATE.read_text(encoding="utf-8")
    for pin in _PINS:
        assert pin in src, f"§10 fuzz target missing: {pin}"
