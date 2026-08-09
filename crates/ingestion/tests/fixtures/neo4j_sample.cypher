// Neo4j sample: graph-native supply chain data
// Simulates what the Neo4j connector's introspect_all() would discover from node labels and relationships.

// Nodes by label: Supplier, Customer, Product, Invoice
CREATE (:Supplier {
  name: "Om Building Materials",
  gstin: "10CQAPS3890L1ZM",
  type: "supplier",
  city: "Patna",
  state: "Bihar"
});

CREATE (:Customer {
  name: "Achintya Industries Pvt. Ltd.",
  gstin: "09AADCA1234C1Z5",
  type: "customer",
  city: "Kanpur",
  state: "Uttar Pradesh"
});

CREATE (:Product {
  name: "Grey Cement",
  hsn_code: "2523291",
  category: "Cement",
  unit_price: 590.00,
  gst_rate: 28.00
});

CREATE (:Product {
  name: "Fe 500 TMT Bar",
  hsn_code: "7214200",
  category: "Steel",
  unit_price: 58500.00,
  gst_rate: 18.00
});

CREATE (:Invoice {
  invoice_number: "INV-2024-001",
  date: "2024-07-15",
  taxable_amount: 714800.00,
  igst_amount: 141644.00,
  total_amount: 856444.00
});

// Relationships
MATCH (s:Supplier {name: "Om Building Materials"})
MATCH (i:Invoice {invoice_number: "INV-2024-001"})
CREATE (s)-[:ISSUED]->(i);

MATCH (i:Invoice {invoice_number: "INV-2024-001"})
MATCH (c:Customer {name: "Achintya Industries Pvt. Ltd."})
CREATE (i)-[:BILLED_TO]->(c);

MATCH (i:Invoice {invoice_number: "INV-2024-001"})
MATCH (p:Product {name: "Grey Cement"})
CREATE (i)-[:INCLUDES {quantity: 220, unit: "Bags", rate: 590.00}]->(p);

MATCH (i:Invoice {invoice_number: "INV-2024-001"})
MATCH (p:Product {name: "Fe 500 TMT Bar"})
CREATE (i)-[:INCLUDES {quantity: 10, unit: "MT", rate: 58500.00}]->(p);

MATCH (s:Supplier {name: "Om Building Materials"})
MATCH (p:Product {name: "Grey Cement"})
CREATE (s)-[:SUPPLIES]->(p);

MATCH (s:Supplier {name: "Om Building Materials"})
MATCH (p:Product {name: "Fe 500 TMT Bar"})
CREATE (s)-[:SUPPLIES]->(p);
