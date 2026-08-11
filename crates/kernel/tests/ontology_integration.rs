//! Ontology E2E integration tests — MRFC-0041.
//!
//! Demonstrates the full ontology pipeline:
//!   1. Define ontology (classes, relationships, mappings)
//!   2. Import heterogeneous data as KOs
//!   3. Conform KOs to ontology
//!   4. Query with ontology-aware aikoql (inheritance, cross-source)
//!   5. Set-based TRAVERSE with ontology relationships
//!
//! Uses in-memory storage — no Docker/containers required.

use aikoql_compiler::parser::compile_with_ontology;
use aikoql_graph::{GraphEngineApi, RelateRequest};
use aikoql_kernel::ir::IrOp;
use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::knowledge::ontology::*;
use aikoql_kernel::lifecycle::schema::SchemaRegistry;
use aikoql_kernel::storage::store::MemoryEngine;
use aikoql_kernel::transaction::kernel::{Kernel, ManualClock, RememberRequest, Subject};
use aikoql_runtime::Interpreter;
use std::sync::Arc;

fn make_kernel() -> Kernel {
    let clock = Arc::new(ManualClock::new(10_000));
    Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xC0FFEE).unwrap()
}

fn make_ko(type_name: &str, _props: Vec<(&str, &str)>, tags: Vec<&str>) -> KnowledgeObject {
    KnowledgeObject::new(
        KOID::ZERO,
        Metadata {
            type_name: type_name.into(),
            tenant: None,
            schema_version: 1,
            tags: tags.into_iter().map(String::from).collect(),
        },
        SecurityDescriptor {
            owner: "test".into(),
            acl: vec![],
            classification: None,
        },
    )
}
fn add_props(mut ko: KnowledgeObject, props: Vec<(&str, &str)>) -> KnowledgeObject {
    for (k, v) in props {
        ko.properties.insert(k.into(), Value::Text(v.into()));
    }
    ko
}

fn make_subject(name: &str) -> Subject {
    Subject::with_roles(name, &["admin"])
}

// ---------------------------------------------------------------------------
// Ontology fixture
// ---------------------------------------------------------------------------

fn enterprise_ontology() -> OntologyRegistry {
    let mut classes = std::collections::BTreeMap::new();
    classes.insert(
        "Person".into(),
        ClassDef {
            name: "Person".into(),
            parent: None,
            description: Some("A human being".into()),
        },
    );
    classes.insert(
        "Employee".into(),
        ClassDef {
            name: "Employee".into(),
            parent: Some("Person".into()),
            description: Some("Employed person".into()),
        },
    );
    classes.insert(
        "Department".into(),
        ClassDef {
            name: "Department".into(),
            parent: None,
            description: Some("Organizational unit".into()),
        },
    );

    let mut relationships = std::collections::BTreeMap::new();
    relationships.insert(
        "belongsTo".into(),
        RelDef {
            name: "belongsTo".into(),
            domain: Some("Employee".into()),
            range: Some("Department".into()),
            cardinality: Some(Cardinality::OneToMany),
            max_count: None,
        },
    );

    let mut property_defs = std::collections::BTreeMap::new();
    property_defs.insert(
        "name".into(),
        PropertyDef {
            name: "name".into(),
            value_type: "Text".into(),
            required: true,
            nullable: false,
        },
    );
    property_defs.insert(
        "dept".into(),
        PropertyDef {
            name: "dept".into(),
            value_type: "Text".into(),
            required: false,
            nullable: false,
        },
    );

    let mut pg_map = std::collections::BTreeMap::new();
    pg_map.insert("employee_id".into(), "name".into());
    pg_map.insert("department".into(), "dept".into());

    let mut mongo_map = std::collections::BTreeMap::new();
    mongo_map.insert("emp_name".into(), "name".into());
    mongo_map.insert("dept_name".into(), "dept".into());

    let mappings = vec![
        MappingEntry {
            source: "postgres".into(),
            physical_type: "employees".into(),
            class: "Employee".into(),
            property_map: pg_map,
        },
        MappingEntry {
            source: "mongodb".into(),
            physical_type: "employee".into(),
            class: "Employee".into(),
            property_map: mongo_map,
        },
        MappingEntry {
            source: "postgres".into(),
            physical_type: "departments".into(),
            class: "Department".into(),
            property_map: std::collections::BTreeMap::new(),
        },
    ];

    OntologyRegistry::new(OntologyDef {
        namespace: "enterprise".into(),
        version: "1.0".into(),
        classes,
        relationships,
        property_defs,
        mappings,
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn e2e_ontology_expands_to_physical_sources() {
    let ont = enterprise_ontology();
    let registry = SchemaRegistry::new();
    let plans =
        compile_with_ontology("MATCH Employee RETURN *", "test", &registry, Some(&ont)).unwrap();
    assert_eq!(plans.len(), 2);
    let types: Vec<&str> = plans
        .iter()
        .filter_map(|p| match &p.operators[0] {
            IrOp::Scan { type_name, .. } => Some(type_name.as_str()),
            _ => None,
        })
        .collect();
    assert!(types.contains(&"employees"));
    assert!(types.contains(&"employee"));
}

#[test]
fn e2e_inheritance_query_returns_subclass_mappings() {
    let ont = enterprise_ontology();
    let registry = SchemaRegistry::new();
    // Person has 0 direct mappings, but Employee (subclass) has 2
    let plans =
        compile_with_ontology("MATCH Person RETURN *", "test", &registry, Some(&ont)).unwrap();
    assert_eq!(plans.len(), 2);
}

#[test]
fn e2e_traverse_validates_against_ontology() {
    let ont = enterprise_ontology();
    let registry = SchemaRegistry::new();
    let plans = compile_with_ontology(
        "MATCH Employee TRAVERSE belongsTo RETURN *",
        "test",
        &registry,
        Some(&ont),
    )
    .unwrap();
    assert!(!plans.is_empty());
    for plan in &plans {
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[1] {
            IrOp::Traverse { rel_type, .. } => assert_eq!(rel_type.as_deref(), Some("belongsTo")),
            _ => panic!("expected Traverse op"),
        }
    }
}

#[test]
fn e2e_unknown_relationship_is_rejected() {
    let ont = enterprise_ontology();
    let registry = SchemaRegistry::new();
    let result = compile_with_ontology(
        "MATCH Employee TRAVERSE bogusRel RETURN *",
        "test",
        &registry,
        Some(&ont),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("AIKOQL1033"));
}

#[test]
fn e2e_conform_renames_properties() {
    let ont = enterprise_ontology();
    // Simulate PG import
    let mut pg_ko = add_props(
        make_ko("employees", vec![], vec!["imported", "postgres"]),
        vec![("employee_id", "Alice"), ("department", "Engineering")],
    );
    conform(&mut pg_ko, &ont);
    assert!(pg_ko.metadata.tags.contains(&"class:Employee".to_string()));
    assert_eq!(
        pg_ko.properties.get("name").and_then(text_val),
        Some("Alice")
    );
    assert_eq!(
        pg_ko.properties.get("dept").and_then(text_val),
        Some("Engineering")
    );
    // Original keys should be gone
    assert!(!pg_ko.properties.contains_key("employee_id"));
    assert!(!pg_ko.properties.contains_key("department"));
}

#[test]
fn e2e_conform_mongo_mapping() {
    let ont = enterprise_ontology();
    let mut mongo_ko = add_props(
        make_ko("employee", vec![], vec!["imported", "mongodb"]),
        vec![("emp_name", "Bob"), ("dept_name", "Engineering")],
    );
    conform(&mut mongo_ko, &ont);
    assert_eq!(
        mongo_ko.properties.get("name").and_then(text_val),
        Some("Bob")
    );
    assert_eq!(
        mongo_ko.properties.get("dept").and_then(text_val),
        Some("Engineering")
    );
}

fn text_val(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

#[test]
fn e2e_full_pipeline_with_kernel() {
    let ont = enterprise_ontology();
    let kernel = make_kernel();
    let subj = make_subject("admin");

    // 1. Create ontology KO
    kernel
        .remember(RememberRequest {
            context: subj.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("ont-e2e".into()),
            metadata: Metadata {
                type_name: ONTOLOGY_TYPE.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: ont.definition().to_property_map(),
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "admin".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: Some("E2E test".into()),
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();

    // 2. Import PG employees (Alice)
    let alice_koid = kernel
        .remember(RememberRequest {
            context: subj.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("e2e-alice".into()),
            metadata: Metadata {
                type_name: "employees".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["imported".into(), "postgres".into()],
            },
            properties: add_props(
                make_ko("employees", vec![], vec![]),
                vec![("employee_id", "Alice"), ("department", "Engineering")],
            )
            .properties,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "admin".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid;

    // 3. Import Mongo employee (Bob)
    let bob_koid = kernel
        .remember(RememberRequest {
            context: subj.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("e2e-bob".into()),
            metadata: Metadata {
                type_name: "employee".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["imported".into(), "mongodb".into()],
            },
            properties: add_props(
                make_ko("employee", vec![], vec![]),
                vec![("emp_name", "Bob"), ("dept_name", "Engineering")],
            )
            .properties,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "admin".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid;

    // 4. Import Department
    let dept_koid = kernel
        .remember(RememberRequest {
            context: subj.clone().into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: Some("e2e-dept".into()),
            metadata: Metadata {
                type_name: "departments".into(),
                tenant: None,
                schema_version: 1,
                tags: vec!["imported".into(), "postgres".into()],
            },
            properties: add_props(
                make_ko("departments", vec![], vec![]),
                vec![("name", "Engineering")],
            )
            .properties,
            semantic: None,
            relationships: vec![],
            security: Some(SecurityDescriptor {
                owner: "admin".into(),
                acl: vec![],
                classification: None,
            }),
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap()
        .koid;

    // 5. Create belongsTo relationships
    kernel
        .relate(RelateRequest {
            context: subj.clone().into(),
            from: alice_koid,
            to: dept_koid,
            rel_type: "belongsTo".into(),
        })
        .unwrap();
    kernel
        .relate(RelateRequest {
            context: subj.clone().into(),
            from: bob_koid,
            to: dept_koid,
            rel_type: "belongsTo".into(),
        })
        .unwrap();

    // 6. Direct physical query — Alice (use admin subject for ACL)
    let plan =
        aikoql_compiler::parser::compile_with_subject("MATCH employees RETURN *", "admin")
            .unwrap();
    let result = Interpreter::execute(&kernel, &plan).unwrap();
    if let aikoql_runtime::RowSet::Objects(kos) = result {
        assert_eq!(kos.len(), 1);
        assert_eq!(kos[0].metadata.type_name, "employees");
    } else {
        panic!("expected Objects");
    }

    // 7. Ontology-aware query — Employee expands to 2 plans (employees + employee)
    let registry = SchemaRegistry::new();
    let plans =
        compile_with_ontology("MATCH Employee RETURN *", "admin", &registry, Some(&ont)).unwrap();
    assert_eq!(plans.len(), 2);
    let mut total = 0;
    for plan in &plans {
        if let aikoql_runtime::RowSet::Objects(kos) =
            Interpreter::execute(&kernel, plan).unwrap()
        {
            total += kos.len();
        }
    }
    assert_eq!(total, 2);

    // 8. Set-based TRAVERSE
    let tra_plan = aikoql_compiler::parser::compile_with_subject(
        "MATCH employees TRAVERSE belongsTo RETURN *",
        "admin",
    )
    .unwrap();
    let result = Interpreter::execute(&kernel, &tra_plan).unwrap();
    if let aikoql_runtime::RowSet::Traversal(hits) = result {
        assert!(hits.iter().any(|(koid, _rt, _d)| *koid == dept_koid));
    } else {
        panic!("expected Traversal");
    }
}

#[test]
fn e2e_no_mappings_returns_direct_scan() {
    let ont = enterprise_ontology();
    let registry = SchemaRegistry::new();
    let plans =
        compile_with_ontology("MATCH Department RETURN *", "test", &registry, Some(&ont)).unwrap();
    assert_eq!(plans.len(), 1);
    match &plans[0].operators[0] {
        IrOp::Scan { type_name, .. } => assert_eq!(type_name, "departments"),
        _ => panic!("expected Scan"),
    }
}
