-- Wave 5 OLAP benchmark schema + deterministic loads.
-- ClickHouse (MergeTree) and StarRocks (hash-distributed) variants of the
-- SAME logical dataset. The executable form lives in the harness
-- (crates/ingestion/tests/wave5_olap.rs — ensure_loaded); this file is the
-- readable spec.

-- ── ClickHouse (HTTP interface, database aikoql_bench) ──────────────────

CREATE DATABASE IF NOT EXISTS aikoql_bench;

CREATE TABLE txn (customer_id UInt32, amount UInt32)
ENGINE = MergeTree ORDER BY customer_id;
CREATE TABLE events (service UInt16, device UInt32, region UInt8, err UInt8,
                     day UInt16, lat UInt16)
ENGINE = MergeTree ORDER BY day;
CREATE TABLE customers (id UInt32, tier UInt8, region UInt8)
ENGINE = MergeTree ORDER BY id;
CREATE TABLE devices (device_id UInt32, customer_id UInt32)
ENGINE = MergeTree ORDER BY device_id;

INSERT INTO txn
SELECT number % 100000, number % 1000 FROM numbers(10000000);
INSERT INTO events
SELECT number % 20, number % 1000, number % 5, number % 100 = 0,
       number / 86400, number % 500 FROM numbers(1000000);
INSERT INTO customers
SELECT number, number % 3, number % 5 FROM numbers(100000);
INSERT INTO devices
SELECT number, number % 100000 FROM numbers(1000000);

-- ── StarRocks (MySQL protocol, database aikoql_bench) ───────────────────

CREATE DATABASE IF NOT EXISTS aikoql_bench;

CREATE TABLE aikoql_bench.seed (v INT) DISTRIBUTED BY HASH(v) BUCKETS 1;
INSERT INTO aikoql_bench.seed VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9);

CREATE TABLE aikoql_bench.txn
  (customer_id INT NOT NULL, amount INT NOT NULL)
  DISTRIBUTED BY HASH(customer_id) BUCKETS 8;
CREATE TABLE aikoql_bench.events
  (service SMALLINT NOT NULL, device INT NOT NULL, region TINYINT NOT NULL,
   err TINYINT NOT NULL, day SMALLINT NOT NULL, lat SMALLINT NOT NULL)
  DISTRIBUTED BY HASH(device) BUCKETS 8;
CREATE TABLE aikoql_bench.customers
  (id INT NOT NULL, tier TINYINT NOT NULL, region TINYINT NOT NULL)
  DISTRIBUTED BY HASH(id) BUCKETS 8;
CREATE TABLE aikoql_bench.devices
  (device_id INT NOT NULL, customer_id INT NOT NULL)
  DISTRIBUTED BY HASH(device_id) BUCKETS 8;

-- numbers(N) is ClickHouse-only. StarRocks generates the same set via a
-- cross join of the 10-row seed (n = 0..10^digits-1 exactly once):
--   INSERT INTO aikoql_bench.txn SELECT n % 100000, n % 1000 FROM
--   (SELECT (a.v*1000000+b.v*100000+c.v*10000+d.v*1000+e.v*100+f.v*10+g.v*1) AS n
--    FROM aikoql_bench.seed a, aikoql_bench.seed b, aikoql_bench.seed c,
--         aikoql_bench.seed d, aikoql_bench.seed e, aikoql_bench.seed f,
--         aikoql_bench.seed g) AS g;
-- (digits 7/6/6/5 for txn/events/devices/customers; the trailing alias is
-- required by StarRocks — every derived table needs one.)
