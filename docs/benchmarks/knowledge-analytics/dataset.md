# Wave 5 OLAP benchmark dataset — deterministic generator + ground truth

Source of truth: `crates/ingestion/tests/wave5_olap.rs` — the loader and the
Rust ground-truth functions ARE the spec; this file is the readable form.
Every engine loads the identical logical dataset. Every value derives from
the row index `n`, never from randomness: a rerun reproduces bit-for-bit.

## Tables

| Table | Rows | Formula | Notes |
| --- | --- | --- | --- |
| txn | 10,000,000 | customer_id = n % 100000, amount = n % 1000 | 100 rows per customer |
| events | 1,000,000 | service = n % 20, device = n % 1000, region = n % 5, err = (n % 100 == 0), day = n / 86400, lat = n % 500 | 1000 events per device, 12 day buckets |
| customers | 100,000 | id = n, tier = n % 3, region = n % 5 | 1 row per customer |
| devices | 1,000,000 | device_id = n, customer_id = n % 100000 | 1 row per device |

Generation: ClickHouse `numbers(N)`; StarRocks uses a cross join of a 10-row
seed table (`sr_numbers(digits)` in the test — pure standard SQL, produces
n = 0..10^digits−1 exactly once).

## Generator coupling (the test-side traps, fixed during RED)

device = n % 1000 pins the other event columns:
service = device % 20, region = device % 5, err = 1 ⟺ device % 100 == 0.
Consequences the spot checks must respect:

- all 10,000 error rows sit in service 0 (n ≡ 0 mod 100 ⟹ n ≡ 0 mod 20);
- err = 1 is impossible with service ≠ 0 or region ≠ 0;
- lat ≡ service (mod 20) because 20 | 500 — each service sees exactly 25
  latency values, 2000 rows each, so p95 = service + 460 (exact).

## Ground truth

- txn grand total: 4,995,000,000 = 100 × 100 × (0+…+999) × 1000 cycles
  (100000 ≡ 0 mod 1000, so per-customer sum = 100 × (c % 1000))
- txn per-tier sums: tier = c % 3 (computed in `txn_truth()`)
- events: 240 (service, day) buckets (20 × 12); 12,000 distinct
  (service, device, region, err, day) combos (1000 device-pinned quadruples
  × 12 days); 10,000 error rows, all service 0; p95 latency per service =
  service + 460
- events service 0 / day 0 count: 4320 (86400 / 20)
- device d: 1000 events, max lat = d % 500 (1000 ≡ 0 mod 500)
