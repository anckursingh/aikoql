# Benchmark Results (pinned baselines)

> generated from TESTING-PLAN.md §9.1 by scripts/certify.js

G10/G11/G12/§32 canonical measurements, pinned in TESTING-PLAN §3 rows 127–130 (regression-guarded weekly in CI, >20% alert). These are the baselines the release gate compares against — not fresh measurements. Each headline below was cross-checked to still appear in the plan.

- **Track-B knowledge bench (§30)**: AikoQL 13/14 vs RAG 9/14 (measured 2026-08-22)
- **Agent efficacy G10 (§31, 51 tasks)**: canonical A 0.059 / B 0.569 / C 0.510 / D 1.000 — 51/51 at 0 LLM calls (measured 2026-08-24)
- **Chatbot comparative G11 (§52)**: accuracy A 0/16, B 10/16, C 13/16, D 15/16 (measured 2026-08-22)
- **Agent memory §32**: D 20/20 vs conventional B 12/20 (measured 2026-08-24)

All pinned headlines present in TESTING-PLAN.md.
