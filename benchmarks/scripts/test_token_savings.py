# benchmarks/scripts/test_token_savings.py
from token_savings import count_tokens, harness

def test_cl100k_counts_are_pinned():
    assert count_tokens("hello world") == 2   # cl100k_base: "hello" + " world"
    assert count_tokens("") == 0
    assert count_tokens("SELECT * FROM notes") == count_tokens("SELECT * FROM notes")

def test_memory_context_is_no_larger_than_baseline():
    r = harness(corpus_dir="fixtures/corpus", queries_file="fixtures/queries.json")
    assert r["memory_tokens"] <= r["baseline_tokens"]
    assert 0 <= r["reduction_pct"] <= 100
