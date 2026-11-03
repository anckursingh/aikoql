# benchmarks/scripts/test_token_savings.py
from pathlib import Path

from token_savings import count_tokens, harness

FIXTURES = Path(__file__).resolve().parent / "fixtures"

def test_cl100k_counts_are_pinned():
    assert count_tokens("hello world") == 2   # cl100k_base: "hello" + " world"
    assert count_tokens("") == 0
    assert count_tokens("SELECT * FROM notes") == count_tokens("SELECT * FROM notes")

def test_memory_context_is_no_larger_than_baseline():
    r = harness(corpus_dir=str(FIXTURES / "corpus"),
                queries_file=str(FIXTURES / "queries.json"))
    assert r["memory_tokens"] <= r["baseline_tokens"]
    assert 0 <= r["reduction_pct"] <= 100
