-- SQLite sample schema: local/offline CRM
-- Simulates what the SQLite connector's introspect_all() would discover.

CREATE TABLE contacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    company TEXT,
    email TEXT UNIQUE,
    phone TEXT,
    type TEXT CHECK(type IN ('supplier', 'customer', 'both')) DEFAULT 'customer',
    gstin TEXT UNIQUE,
    created_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE invoices (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    invoice_number TEXT UNIQUE NOT NULL,
    contact_id INTEGER NOT NULL REFERENCES contacts(id),
    issue_date TEXT NOT NULL,
    due_date TEXT,
    taxable_amount REAL,
    igst_amount REAL,
    total_amount REAL NOT NULL,
    payment_status TEXT DEFAULT 'unpaid',
    created_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE line_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    invoice_id INTEGER NOT NULL REFERENCES invoices(id),
    description TEXT NOT NULL,
    hsn_code TEXT,
    quantity REAL NOT NULL,
    unit TEXT,
    rate REAL NOT NULL,
    gst_rate REAL,
    amount REAL NOT NULL
);

-- Sample data
INSERT INTO contacts (name, company, email, phone, type, gstin) VALUES
('Ramesh Kumar Agarwal', 'Om Building Materials', 'ramesh@ombuilding.com', '0612-2345678', 'supplier', '10CQAPS3890L1ZM'),
('Achintya Industries Pvt. Ltd.', 'Achintya Industries Pvt. Ltd.', 'info@achintya.com', '0512-2567890', 'customer', '09AADCA1234C1Z5');
