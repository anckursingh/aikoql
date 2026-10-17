"""T-04 RED — the question-template engine (FZ-T4, design §11).

Entity names are user content: a name containing quotes, backslashes,
unicode or control characters must reach the question only in escaped
form — a ref always renders as one quoted token, and no control
character survives. The same engine owns relation phrasing (the verb
map), so multi-hop questions conjugate correctly.

FZ-T4 property: arbitrary names escape and round-trip — nothing
reaches the question raw.
"""

from hypothesis import assume, given
from hypothesis import strategies as st

from aikoql_training.scenarios.templates import ref_of

_NAMES = st.text(
    alphabet=st.characters(
        min_codepoint=32, max_codepoint=0x2FFF, blacklist_categories=("Cs", "Cc")
    ),
    max_size=20,
)


def _unescape(s):
    return s.replace("\\'", "'").replace("\\\\", "\\")


def test_quote_in_name_is_escaped():
    ref = ref_of(
        "a" * 32,
        {"a" * 32: {"type_name": "service", "properties": {"name": "O'Reilly"}}},
    )
    assert ref == "service 'O\\'Reilly'"


def test_control_chars_never_reach_the_question():
    ref = ref_of(
        "a" * 32,
        {"a" * 32: {"type_name": "service",
                    "properties": {"name": "bad\x00\x1fname\x7f"}}},
    )
    assert all(ord(ch) >= 32 for ch in ref)
    assert "\x00" not in ref and "\x1f" not in ref and "\x7f" not in ref


def test_aikoql_keyword_names_render_as_quoted_tokens():
    ref = ref_of(
        "a" * 32,
        {"a" * 32: {"type_name": "service", "properties": {"name": "MATCH"}}},
    )
    assert ref == "service 'MATCH'"  # a label, never interpreted


def test_blank_name_falls_back_to_type_and_prefix():
    ref = ref_of(
        "a" * 32,
        {"a" * 32: {"type_name": "service", "properties": {"name": "   "}}},
    )
    assert ref == "service aaaaaaaa"


@given(_NAMES)
def test_ref_escapes_round_trip(name):
    assume(name.strip())
    ko = {"a" * 32: {"type_name": "service", "properties": {"name": name}}}
    ref = ref_of("a" * 32, ko)
    assert all(ord(ch) >= 32 for ch in ref)
    assert ref.endswith("'")
    # The inner token unescapes back to the original name exactly.
    token = ref[len("service '"):-1]
    assert _unescape(token) == name
