-- Postgres sample schema: e-commerce/CRM database
-- Simulates what the Postgres connector's introspect_all() would discover.

CREATE TABLE customers (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    email VARCHAR(255) UNIQUE NOT NULL,
    phone VARCHAR(20),
    address TEXT,
    gstin VARCHAR(15),
    created_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW()
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    order_number VARCHAR(50) UNIQUE NOT NULL,
    order_date DATE NOT NULL,
    status VARCHAR(20) DEFAULT 'pending',
    taxable_amount NUMERIC(12,2),
    igst_amount NUMERIC(12,2),
    total_amount NUMERIC(12,2) NOT NULL,
    created_at TIMESTAMP DEFAULT NOW()
);

CREATE TABLE order_items (
    id SERIAL PRIMARY KEY,
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product_name VARCHAR(255) NOT NULL,
    hsn_code VARCHAR(8),
    quantity NUMERIC(10,2) NOT NULL,
    unit VARCHAR(20),
    unit_price NUMERIC(10,2) NOT NULL,
    gst_rate NUMERIC(5,2),
    line_total NUMERIC(12,2) NOT NULL
);

CREATE TABLE products (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    hsn_code VARCHAR(8),
    category VARCHAR(100),
    unit_price NUMERIC(10,2),
    gst_rate NUMERIC(5,2) DEFAULT 18.00,
    stock_quantity INTEGER DEFAULT 0
);

-- Sample data
INSERT INTO customers (name, email, phone, address, gstin) VALUES
('Om Building Materials', 'sales@ombuilding.com', '0612-2345678', 'Shop No. 12, Gandhi Nagar, Patna, Bihar - 800001', '10CQAPS3890L1ZM'),
('Achintya Industries Pvt. Ltd.', 'info@achintya.com', '0512-2567890', 'Plot 45, Industrial Area, Kanpur, UP - 208001', '09AADCA1234C1Z5');

INSERT INTO products (name, hsn_code, category, unit_price, gst_rate) VALUES
('Grey Cement', '2523291', 'Cement', 590.00, 28.00),
('Fe 500 TMT Bar', '7214200', 'Steel', 58500.00, 18.00);
