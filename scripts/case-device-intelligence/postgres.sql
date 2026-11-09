-- ILLUSTRATIVE CRM for the device-intelligence case study (docs/ONTOLOGY-IN-PRACTICE.md).
CREATE TABLE accounts (
  id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK (status IN ('active','suspended','closed')),
  plan TEXT, country TEXT, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO accounts
SELECT 'acct_' || lpad(g::text, 4, '0'), 'user' || g || '@example.test', 'active',
       (ARRAY['free','pro','enterprise'])[g % 3 + 1], (ARRAY['NO','US','IN','DE'])[g % 4 + 1], now() - (g || ' days')::interval
FROM generate_series(0, 999) g;
UPDATE accounts SET status = 'suspended' WHERE id IN ('acct_0011','acct_0012');
