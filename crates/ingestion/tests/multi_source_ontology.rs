//! Multi-source ontology discovery integration test.
//!
//! Demonstrates `merge_proposals()` combining ontology proposals from:
//! - Document IR (simulating invoice entity extraction)
//! - Postgres schema introspect (customers, orders, products tables)
//! - MongoDB document sampling (suppliers, purchase_orders collections)
//! - SQLite schema introspect (contacts, invoices, line_items tables)
//! - Neo4j graph labels + relationships (Supplier, Customer, Product, Invoice)
//!
//! Fixture files in `tests/fixtures/` show the schemas these proposals simulate.

use aikoql_ingestion::{
    merge_proposals, ClassProposal, Evidence, OntologyProposal, PropertyProposal,
    RelationshipProposal,
};

// ── Helpers ──

fn evidence(source: &str, confidence: f32) -> Evidence {
    Evidence {
        document_id: Some(source.into()),
        page: None,
        bbox_text: Some(format!("discovered in {}", source)),
        extractor: format!("{}-introspect", source),
        model: Some("mock-v1".into()),
        confidence,
    }
}

fn class_proposal(
    name: &str,
    parent: Option<&str>,
    confidence: f32,
    source: &str,
) -> ClassProposal {
    ClassProposal {
        name: name.into(),
        parent: parent.map(|s| s.into()),
        description: Some(format!("discovered from {}", source)),
        signal_count: 1,
        confidence,
        evidence: vec![evidence(source, confidence)],
    }
}

fn prop_proposal(
    name: &str,
    class_name: &str,
    value_type: &str,
    source: &str,
    confidence: f32,
) -> PropertyProposal {
    PropertyProposal {
        name: name.into(),
        class_name: class_name.into(),
        value_type: value_type.into(),
        required: false,
        confidence,
        evidence: vec![evidence(source, confidence)],
    }
}

fn rel_proposal(
    name: &str,
    domain: Option<&str>,
    range: Option<&str>,
    source: &str,
) -> RelationshipProposal {
    RelationshipProposal {
        name: name.into(),
        domain: domain.map(|s| s.into()),
        range: range.map(|s| s.into()),
        cardinality: Some("1:N".into()),
        confidence: 0.9,
        evidence: vec![evidence(source, 0.9)],
    }
}

// ── Source-specific proposal builders ──

fn postgres_proposal() -> OntologyProposal {
    OntologyProposal {
        method: "postgres-connector".into(),
        document_id: Some("pg:information_schema".into()),
        classes: vec![
            class_proposal("Customer", Some("Organization"), 1.0, "pg:customers"),
            class_proposal("Order", None, 1.0, "pg:orders"),
            class_proposal("OrderItem", None, 1.0, "pg:order_items"),
            class_proposal("Product", None, 1.0, "pg:products"),
        ],
        properties: vec![
            prop_proposal("name", "Customer", "Text", "pg:customers", 1.0),
            prop_proposal("email", "Customer", "Text", "pg:customers", 1.0),
            prop_proposal("gstin", "Customer", "Text", "pg:customers", 1.0),
            prop_proposal("address", "Customer", "Text", "pg:customers", 0.9),
            prop_proposal("order_number", "Order", "Text", "pg:orders", 1.0),
            prop_proposal("order_date", "Order", "Date", "pg:orders", 1.0),
            prop_proposal("status", "Order", "Text", "pg:orders", 0.9),
            prop_proposal("taxable_amount", "Order", "Decimal", "pg:orders", 1.0),
            prop_proposal("total_amount", "Order", "Decimal", "pg:orders", 1.0),
            prop_proposal("igst_amount", "Order", "Decimal", "pg:orders", 0.8),
            prop_proposal("product_name", "OrderItem", "Text", "pg:order_items", 1.0),
            prop_proposal("hsn_code", "OrderItem", "Text", "pg:order_items", 0.9),
            prop_proposal("quantity", "OrderItem", "Decimal", "pg:order_items", 1.0),
            prop_proposal("unit_price", "OrderItem", "Decimal", "pg:order_items", 1.0),
            prop_proposal("gst_rate", "Product", "Decimal", "pg:products", 0.9),
        ],
        relationships: vec![
            rel_proposal(
                "placed_by",
                Some("Order"),
                Some("Customer"),
                "pg:orders.customer_id",
            ),
            rel_proposal(
                "contains",
                Some("Order"),
                Some("OrderItem"),
                "pg:order_items.order_id",
            ),
        ],
    }
}

fn mongo_proposal() -> OntologyProposal {
    OntologyProposal {
        method: "mongodb-connector".into(),
        document_id: Some("mongo:collections".into()),
        classes: vec![
            class_proposal("Supplier", Some("Organization"), 0.95, "mongo:suppliers"),
            class_proposal("PurchaseOrder", None, 0.9, "mongo:purchase_orders"),
        ],
        properties: vec![
            prop_proposal("name", "Supplier", "Text", "mongo:suppliers", 0.95),
            prop_proposal("gstin", "Supplier", "Text", "mongo:suppliers", 0.95),
            prop_proposal(
                "bank",
                "Supplier",
                "Text",
                "mongo:suppliers.bank_account",
                0.8,
            ),
            prop_proposal(
                "account_no",
                "Supplier",
                "Text",
                "mongo:suppliers.bank_account",
                0.8,
            ),
            prop_proposal(
                "order_number",
                "PurchaseOrder",
                "Text",
                "mongo:purchase_orders",
                0.9,
            ),
            prop_proposal(
                "taxable_amount",
                "PurchaseOrder",
                "Decimal",
                "mongo:purchase_orders",
                0.9,
            ),
            prop_proposal(
                "total_amount",
                "PurchaseOrder",
                "Decimal",
                "mongo:purchase_orders",
                0.9,
            ),
        ],
        relationships: vec![rel_proposal(
            "placed_by",
            Some("PurchaseOrder"),
            Some("Supplier"),
            "mongo:supplier_name",
        )],
    }
}

fn sqlite_proposal() -> OntologyProposal {
    OntologyProposal {
        method: "sqlite-connector".into(),
        document_id: Some("sqlite:sqlite_master".into()),
        classes: vec![
            class_proposal("Contact", Some("Organization"), 1.0, "sqlite:contacts"),
            class_proposal("Invoice", None, 1.0, "sqlite:invoices"),
            class_proposal("LineItem", None, 1.0, "sqlite:line_items"),
        ],
        properties: vec![
            prop_proposal("name", "Contact", "Text", "sqlite:contacts", 1.0),
            prop_proposal("company", "Contact", "Text", "sqlite:contacts", 0.9),
            prop_proposal("gstin", "Contact", "Text", "sqlite:contacts", 1.0),
            prop_proposal("invoice_number", "Invoice", "Text", "sqlite:invoices", 1.0),
            prop_proposal("issue_date", "Invoice", "Date", "sqlite:invoices", 1.0),
            prop_proposal("payment_status", "Invoice", "Text", "sqlite:invoices", 0.9),
            prop_proposal(
                "taxable_amount",
                "Invoice",
                "Decimal",
                "sqlite:invoices",
                1.0,
            ),
            prop_proposal("total_amount", "Invoice", "Decimal", "sqlite:invoices", 1.0),
            prop_proposal("description", "LineItem", "Text", "sqlite:line_items", 1.0),
            prop_proposal("hsn_code", "LineItem", "Text", "sqlite:line_items", 0.9),
            prop_proposal("rate", "LineItem", "Decimal", "sqlite:line_items", 1.0),
        ],
        relationships: vec![
            rel_proposal(
                "issued_to",
                Some("Invoice"),
                Some("Contact"),
                "sqlite:invoices.contact_id",
            ),
            rel_proposal(
                "has_item",
                Some("Invoice"),
                Some("LineItem"),
                "sqlite:line_items.invoice_id",
            ),
        ],
    }
}

fn neo4j_proposal() -> OntologyProposal {
    OntologyProposal {
        method: "neo4j-connector".into(),
        document_id: Some("neo4j:labels".into()),
        classes: vec![
            class_proposal("Supplier", Some("Organization"), 0.95, "neo4j:Supplier"),
            class_proposal("Customer", Some("Organization"), 0.95, "neo4j:Customer"),
            class_proposal("Product", None, 1.0, "neo4j:Product"),
            class_proposal("Invoice", None, 1.0, "neo4j:Invoice"),
        ],
        properties: vec![
            prop_proposal("name", "Supplier", "Text", "neo4j:Supplier", 0.95),
            prop_proposal("gstin", "Supplier", "Text", "neo4j:Supplier", 0.95),
            prop_proposal("hsn_code", "Product", "Text", "neo4j:Product", 0.9),
            prop_proposal("category", "Product", "Text", "neo4j:Product", 0.8),
            prop_proposal("unit_price", "Product", "Decimal", "neo4j:Product", 0.9),
            prop_proposal("gst_rate", "Product", "Decimal", "neo4j:Product", 0.9),
            prop_proposal("invoice_number", "Invoice", "Text", "neo4j:Invoice", 0.9),
            prop_proposal("date", "Invoice", "Date", "neo4j:Invoice", 0.8),
            prop_proposal("taxable_amount", "Invoice", "Decimal", "neo4j:Invoice", 0.9),
            prop_proposal("total_amount", "Invoice", "Decimal", "neo4j:Invoice", 0.9),
        ],
        relationships: vec![
            rel_proposal("ISSUED", Some("Supplier"), Some("Invoice"), "neo4j:ISSUED"),
            rel_proposal(
                "BILLED_TO",
                Some("Invoice"),
                Some("Customer"),
                "neo4j:BILLED_TO",
            ),
            rel_proposal(
                "INCLUDES",
                Some("Invoice"),
                Some("Product"),
                "neo4j:INCLUDES",
            ),
            rel_proposal(
                "SUPPLIES",
                Some("Supplier"),
                Some("Product"),
                "neo4j:SUPPLIES",
            ),
        ],
    }
}

fn document_proposal() -> OntologyProposal {
    OntologyProposal {
        method: "document-ir".into(),
        document_id: Some("doc:invoice_9655.pdf".into()),
        classes: vec![
            class_proposal("Organization", None, 0.85, "doc:entity names"),
            class_proposal("Invoice", None, 0.9, "doc:TAX INVOICE"),
            class_proposal("Product", None, 0.75, "doc:line items"),
            class_proposal("BankAccount", None, 0.7, "doc:bank details"),
        ],
        properties: vec![
            prop_proposal("name", "Organization", "Text", "doc:entity names", 0.85),
            prop_proposal("gstin", "Organization", "Text", "doc:GSTIN", 0.9),
            prop_proposal("address", "Organization", "Text", "doc:address", 0.8),
            prop_proposal("invoice_number", "Invoice", "Text", "doc:Invoice No", 0.9),
            prop_proposal("date", "Invoice", "Date", "doc:Date", 0.85),
            prop_proposal("taxable_amount", "Invoice", "Decimal", "doc:Taxable", 0.9),
            prop_proposal("total_amount", "Invoice", "Decimal", "doc:GRAND TOTAL", 0.9),
            prop_proposal("igst_amount", "Invoice", "Decimal", "doc:IGST", 0.85),
            prop_proposal("product_name", "Product", "Text", "doc:line items", 0.75),
            prop_proposal("hsn_code", "Product", "Text", "doc:HSN", 0.85),
            prop_proposal("quantity", "Product", "Decimal", "doc:qty", 0.8),
            prop_proposal("unit_price", "Product", "Decimal", "doc:rate", 0.8),
            prop_proposal("bank", "BankAccount", "Text", "doc:Bank", 0.7),
            prop_proposal("account_no", "BankAccount", "Text", "doc:Account No", 0.7),
            prop_proposal("ifsc", "BankAccount", "Text", "doc:IFSC", 0.7),
        ],
        relationships: vec![
            rel_proposal(
                "issued_by",
                Some("Invoice"),
                Some("Organization"),
                "doc:seller",
            ),
            rel_proposal(
                "billed_to",
                Some("Invoice"),
                Some("Organization"),
                "doc:buyer",
            ),
            rel_proposal(
                "includes",
                Some("Invoice"),
                Some("Product"),
                "doc:line items",
            ),
        ],
    }
}

// ── Tests ──

#[test]
fn multi_source_merge_combines_all_sources() {
    let sources = vec![
        document_proposal(),
        postgres_proposal(),
        mongo_proposal(),
        sqlite_proposal(),
        neo4j_proposal(),
    ];

    let merged = merge_proposals(&sources);

    assert!(
        merged.classes.len() >= 4,
        "merged should have at least 4 distinct classes, got {}",
        merged.classes.len()
    );
    assert!(
        merged.properties.len() > 10,
        "merged should have substantial property set, got {}",
        merged.properties.len()
    );
    assert!(
        merged.relationships.len() >= 4,
        "merged should have multiple relationships, got {}",
        merged.relationships.len()
    );
}

#[test]
fn merge_dedupes_duplicate_classes() {
    let sources = vec![
        document_proposal(),
        postgres_proposal(),
        mongo_proposal(),
        sqlite_proposal(),
        neo4j_proposal(),
    ];

    let merged = merge_proposals(&sources);

    // Organization appears as a direct class in document_proposal and as parent
    // of Customer/Supplier/Contact in DB proposals. Verify it exists exactly once.
    let org_count = merged
        .classes
        .iter()
        .filter(|c| c.name == "Organization")
        .count();
    assert_eq!(org_count, 1, "Organization should be deduplicated");

    // Invoice appears in document, sqlite, and neo4j proposals.
    let inv_count = merged
        .classes
        .iter()
        .filter(|c| c.name == "Invoice")
        .count();
    assert_eq!(inv_count, 1, "Invoice should be deduplicated");
    let inv = merged.classes.iter().find(|c| c.name == "Invoice").unwrap();
    assert!(
        inv.signal_count >= 3,
        "Invoice signal_count from 3 sources, got {}",
        inv.signal_count
    );
}

#[test]
fn merge_averages_confidences() {
    let sources = vec![document_proposal(), neo4j_proposal()];
    let merged = merge_proposals(&sources);

    // Both propose Invoice — confidence should be averaged.
    let inv = merged.classes.iter().find(|c| c.name == "Invoice").unwrap();
    assert!(
        inv.confidence > 0.89 && inv.confidence < 1.0,
        "confidence should average, got {}",
        inv.confidence
    );
    assert!(inv.signal_count >= 2);
}

#[test]
fn merge_preserves_across_document_and_db_sources() {
    let sources = vec![document_proposal(), mongo_proposal()];
    let merged = merge_proposals(&sources);

    // bank and account_no from document + mongo.
    assert!(merged.properties.iter().any(|p| p.name == "bank"));
    assert!(merged.properties.iter().any(|p| p.name == "account_no"));
    // BankAccount class only from document — survives merge.
    assert!(merged.classes.iter().any(|c| c.name == "BankAccount"));
}

#[test]
fn merge_produces_complete_ontology_picture() {
    let sources = vec![
        document_proposal(),
        postgres_proposal(),
        mongo_proposal(),
        sqlite_proposal(),
        neo4j_proposal(),
    ];
    let merged = merge_proposals(&sources);

    let class_names: Vec<&str> = merged.classes.iter().map(|c| c.name.as_str()).collect();
    for expected in &[
        "Organization",
        "Invoice",
        "Product",
        "Customer",
        "Supplier",
        "Order",
        "Contact",
    ] {
        assert!(
            class_names.contains(expected),
            "missing class: {}",
            expected
        );
    }

    let prop_names: Vec<&str> = merged.properties.iter().map(|p| p.name.as_str()).collect();
    for expected in &[
        "name",
        "gstin",
        "invoice_number",
        "taxable_amount",
        "total_amount",
        "hsn_code",
    ] {
        assert!(
            prop_names.contains(expected),
            "missing property: {}",
            expected
        );
    }

    let rel_names: Vec<&str> = merged
        .relationships
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    for expected in &["ISSUED", "BILLED_TO", "SUPPLIES", "issued_to", "placed_by"] {
        assert!(
            rel_names.contains(expected),
            "missing relationship: {}",
            expected
        );
    }
}
