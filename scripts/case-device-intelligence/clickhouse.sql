-- ILLUSTRATIVE device-intelligence schema built from public concepts only
-- (browser/OS/screen/timezone components, visits, risk decisions). Not any vendor's real schema.
DROP DATABASE IF EXISTS device_intel;
CREATE DATABASE device_intel;
CREATE TABLE device_intel.devices (
  visitor_id String, first_seen DateTime, updated_at DateTime,
  browser LowCardinality(String), os LowCardinality(String), screen String, timezone String,
  language LowCardinality(String), incognito Bool, hardware_concurrency UInt8,
  risk_score Float32, fingerprint_hash String
) ENGINE = ReplacingMergeTree(updated_at) ORDER BY visitor_id;
INSERT INTO device_intel.devices
SELECT concat('vis_', lpad(toString(number), 5, '0')),
       now() - toIntervalDay(number % 90), now() - toIntervalDay(30),
       ['Chrome','Safari','Firefox','Edge'][number % 4 + 1],
       ['Windows','macOS','Android','iOS','Linux'][number % 5 + 1],
       ['1920x1080','1440x900','390x844','2560x1440'][number % 4 + 1],
       ['Europe/Oslo','America/New_York','Asia/Kolkata','UTC'][number % 4 + 1],
       ['en-US','nb-NO','hi-IN'][number % 3 + 1],
       number % 17 = 0, [4,8,12][number % 3 + 1], (number % 100) / 100,
       hex(MD5(concat(toString(number % 4), toString(number % 5), toString(number % 3), toString(number % 7))))
FROM numbers(3000);
-- a later version of 50 devices: their risk rose (the latest row per visitor_id wins)
INSERT INTO device_intel.devices
SELECT visitor_id, first_seen, now(), browser, os, screen, timezone, language, incognito, hardware_concurrency, 0.97, fingerprint_hash
FROM device_intel.devices WHERE toUInt32(substring(visitor_id, 5)) < 50;
CREATE TABLE device_intel.visits (
  request_id String, visitor_id String, ts DateTime, ip String, account_id String, url_path LowCardinality(String)
) ENGINE = MergeTree ORDER BY (visitor_id, ts);
INSERT INTO device_intel.visits
SELECT toString(generateUUIDv4()), concat('vis_', lpad(toString(number % 3000), 5, '0')),
       now() - toIntervalSecond(number * 7), concat('203.0.113.', toString(number % 40)),
       concat('acct_', lpad(toString(intDiv(number % 3000, 3)), 4, '0')),
       ['/login','/checkout','/profile','/settings'][number % 4 + 1]
FROM numbers(200000);
-- device vis_00007 is used by five different accounts: the shared-device pattern a fraud team hunts
INSERT INTO device_intel.visits
SELECT toString(generateUUIDv4()), 'vis_00007', now() - toIntervalSecond(number * 60), '198.51.100.9',
       concat('acct_', lpad(toString(number % 5 + 1), 4, '0')), '/login'
FROM numbers(500);
CREATE TABLE device_intel.risk_decisions (
  decision_id String, visitor_id String, account_id String, decision LowCardinality(String), reason String, decided_at DateTime
) ENGINE = MergeTree ORDER BY decided_at;
INSERT INTO device_intel.risk_decisions
SELECT toString(generateUUIDv4()), concat('vis_', lpad(toString(number), 5, '0')), concat('acct_', lpad(toString(intDiv(number, 3)), 4, '0')),
       ['allow','challenge','block'][number % 3 + 1],
       ['velocity','shared device','known-bad ip'][number % 3 + 1], now() - toIntervalHour(number)
FROM numbers(600);
-- the risk engine blocks the device behind account acct_0077, which the CRM still shows as active
INSERT INTO device_intel.risk_decisions VALUES (toString(generateUUIDv4()), 'vis_00231', 'acct_0077', 'block', 'account takeover signals', now());
CREATE VIEW device_intel.device_activity AS
SELECT visitor_id, min(ts) AS first_visit, max(ts) AS last_visit, count() AS visit_count,
       uniqExact(ip) AS distinct_ips, uniqExact(account_id) AS distinct_accounts
FROM device_intel.visits GROUP BY visitor_id;
