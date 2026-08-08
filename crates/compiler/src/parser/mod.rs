//! AIKOQL Text Parser — Lexer → AST → KIR per MRFC-0010.
//!
//! Entry point: `compile(source)` — tokenizes, parses, and compiles to `IrPlan`.

pub mod ast;
pub mod diagnostics;
pub mod lexer;
pub mod parser;

use mnemosyne_kernel::ir::*;
use mnemosyne_kernel::knowledge::ontology::OntologyRegistry;
use mnemosyne_kernel::lifecycle::schema::SchemaRegistry;
use mnemosyne_kernel::Value;
use parser::Parser;

/// Parse AIKOQL source into an AST (without compiling to IR).
/// Useful for statement-type dispatch (CREATE executes directly, MATCH compiles to IR).
pub fn parse(source: &str) -> Result<ast::Statement, String> {
    let mut p = Parser::new(source);
    p.parse_statement().map_err(|e| e.to_string())
}

/// Compile AIKOQL source text into a validated `IrPlan`.
pub fn compile(source: &str) -> Result<IrPlan, String> {
    compile_with_subject(source, "query-user")
}

/// Compile with an explicit subject for ACL resolution.
pub fn compile_with_subject(source: &str, subject: &str) -> Result<IrPlan, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    ast_to_ir(&stmt, subject)
}

/// Compile with schema validation (MRFC-0010 §3 Semantic Analyzer).
/// Resolves entities and properties against the SchemaRegistry before KIR compilation.
pub fn compile_with_schema(
    source: &str,
    subject: &str,
    registry: &SchemaRegistry,
) -> Result<IrPlan, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    crate::semantic::SemanticAnalyzer::new(registry)
        .analyze(&stmt)
        .map_err(|e| e.to_string())?;
    ast_to_ir(&stmt, subject)
}

/// Compile with ontology-aware reasoning (MRFC-0041).
///
/// Parses, validates against both SchemaRegistry and OntologyRegistry,
/// compiles to IR, then expands supertype scans: if the MATCH entity is an
/// ontology class with physical mappings, one plan is produced per mapping.
/// If no ontology is provided, behaves identically to `compile_with_schema`.
pub fn compile_with_ontology(
    source: &str,
    subject: &str,
    registry: &SchemaRegistry,
    ontology: Option<&OntologyRegistry>,
) -> Result<Vec<IrPlan>, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    crate::semantic::SemanticAnalyzer::new(registry)
        .with_ontology(ontology.unwrap_or(&OntologyRegistry::empty()))
        .analyze(&stmt)
        .map_err(|e| e.to_string())?;

    let plan = ast_to_ir(&stmt, subject)?;

    // Ontology expansion: if the Scan entity is an ontology class with
    // physical mappings, clone one plan per mapping, substituting the
    // physical type_name for the Scan operator.
    match (&stmt, ontology) {
        (ast::Statement::Match(m), Some(ont)) => {
            let pts = ont.physical_types_for_class(&m.entity);
            if pts.is_empty() {
                Ok(vec![plan])
            } else {
                pts.into_iter().map(|(_source, physical_type)| {
                    let mut plan = plan.clone();
                    // Replace the Scan's type_name with the physical type.
                    for op in &mut plan.operators {
                        if let IrOp::Scan { ref mut type_name, .. } = op {
                            *type_name = physical_type.clone();
                            break;
                        }
                    }
                    plan.description = Some(format!(
                        "MATCH {} (mapped to {})", m.entity, physical_type
                    ));
                    Ok(plan)
                }).collect()
            }
        }
        _ => Ok(vec![plan]),
    }
}

fn ast_to_ir(stmt: &ast::Statement, subject: &str) -> Result<IrPlan, String> {
    match stmt {
        ast::Statement::Match(m) => compile_match(m, subject),
        ast::Statement::Create(c) => compile_create(c, subject),
        ast::Statement::Update(u) => compile_update(u, subject),
        ast::Statement::Delete(d) => compile_delete(d, subject),
        ast::Statement::Ingest(_) => Err("INGEST not yet supported in KIR".into()),
    }
}

fn compile_create(c: &ast::CreateStatement, subject: &str) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![IrOp::Scan {
        type_name: c.entity.clone(),
        subject: subject.into(),
    }])
    .with_description(format!("CREATE {}", c.entity));
    Ok(plan)
}

fn compile_update(u: &ast::UpdateStatement, subject: &str) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![IrOp::Scan {
        type_name: u.entity.clone(),
        subject: subject.into(),
    }])
    .with_description(format!("UPDATE {} {}", u.entity, u.koid));
    Ok(plan)
}

fn compile_delete(d: &ast::DeleteStatement, subject: &str) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![IrOp::Scan {
        type_name: d.entity.clone(),
        subject: subject.into(),
    }])
    .with_description(format!("DELETE {} {}", d.entity, d.koid));
    Ok(plan)
}

fn compile_match(m: &ast::MatchStatement, subject: &str) -> Result<IrPlan, String> {
    let mut ops = Vec::new();

    // Scan.
    ops.push(IrOp::Scan {
        type_name: m.entity.clone(),
        subject: subject.into(),
    });

    // Predicates → Filter.
    let flat = flatten_predicates(&m.predicates);
    if !flat.is_empty() {
        ops.push(IrOp::Filter { predicates: flat });
    }

    // SIMILAR → AnnSearch.
    if let Some(ref sim) = m.similarity {
        ops.push(IrOp::TextSearch {
            query: sim.query.clone(),
            k: 10,
        });
    }

    // TRAVERSE — set-based, consumes Scan output.
    let has_traverse = m.traverse.is_some();
    if let Some(ref trav) = m.traverse {
        ops.push(IrOp::Traverse {
            start_koid: String::new(), // empty = set-based: consume input RowSet
            rel_type: Some(trav.relation.clone()),
            depth: 1,
        });
    }

    // Projection (RETURN clause) — filter properties to requested fields.
    // ponytail: skip Project when Traverse is present — Traverse output is
    // (koid, rel_type, depth) tuples, not KnowledgeObjects. Add KO loading
    // after Traverse when RETURN field projection is needed.
    if !has_traverse {
        match &m.projection {
            ast::Projection::Star => {} // no Project needed — return all fields
            ast::Projection::Explain => {} // handled by explain_endpoint separately
            ast::Projection::Fields(fields) => {
                ops.push(IrOp::Project { fields: fields.clone() });
            }
        }
    }

    let plan = IrPlan::new(ops).with_description(format!("MATCH {}", m.entity));
    plan.validate().map_err(|e| format!("AIKOQL1014: conflicting clauses — {}", e))?;
    Ok(plan)
}

/// Flatten nested AND/OR predicates into a flat list.
fn flatten_predicates(preds: &[ast::Predicate]) -> Vec<Predicate> {
    let mut out = Vec::new();
    for p in preds {
        match p {
            ast::Predicate::Eq { property, value } => {
                out.push(Predicate::eq(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::Neq { property, value } => {
                out.push(Predicate::neq(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::Gt { property, value } => {
                out.push(Predicate::gt(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::Lt { property, value } => {
                out.push(Predicate::lt(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::Gte { property, value } => {
                out.push(Predicate::gte(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::Lte { property, value } => {
                out.push(Predicate::lte(property.clone(), expr_to_value(value)));
            }
            ast::Predicate::And { left, right } => {
                out.extend(flatten_predicates(&[left.as_ref().clone(), right.as_ref().clone()]));
            }
            ast::Predicate::Or { .. } => {
                // ponytail: OR predicates not yet mapped to IR. Each OR branch
                // would produce a separate filter scan; add when needed.
            }
        }
    }
    out
}

fn expr_to_value(e: &ast::Expr) -> Value {
    match e {
        ast::Expr::String(s) => Value::Text(s.clone()),
        ast::Expr::Number(n) => Value::Float(*n),
        ast::Expr::Bool(b) => Value::Bool(*b),
        ast::Expr::Null => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_simple_match() {
        let plan = compile("MATCH Person RETURN *").unwrap();
        assert_eq!(plan.operators.len(), 1);
        match &plan.operators[0] {
            IrOp::Scan { type_name, .. } => assert_eq!(type_name, "Person"),
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn compile_match_with_filter() {
        let plan = compile("MATCH Person WHERE company == \"Visa\" RETURN *").unwrap();
        assert_eq!(plan.operators.len(), 2); // Scan + Filter
    }

    #[test]
    fn compile_match_with_similar() {
        let plan = compile("MATCH Person SIMILAR TO \"John\" RETURN *").unwrap();
        // Scan + TextSearch
        assert_eq!(plan.operators.len(), 2);
    }

    #[test]
    fn compile_match_with_traverse() {
        let plan = compile("MATCH Person TRAVERSE knows RETURN *").unwrap();
        // Scan + Traverse (set-based, empty start_koid)
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[0] {
            IrOp::Scan { type_name, .. } => assert_eq!(type_name, "Person"),
            _ => panic!("expected Scan as first op"),
        }
        match &plan.operators[1] {
            IrOp::Traverse { start_koid, rel_type, depth } => {
                assert!(start_koid.is_empty(), "set-based Traverse uses empty start_koid");
                assert_eq!(rel_type.as_deref(), Some("knows"));
                assert_eq!(*depth, 1);
            }
            _ => panic!("expected Traverse as second op"),
        }
    }

    #[test]
    fn ontology_expands_to_physical_scans() {
        use mnemosyne_kernel::knowledge::ontology::*;
        let mut classes = std::collections::BTreeMap::new();
        classes.insert("Employee".into(), ClassDef {
            name: "Employee".into(), parent: None, description: None,
        });
        let ont = OntologyRegistry::new(OntologyDef {
            namespace: "test".into(), version: "1".into(),
            classes,
            relationships: std::collections::BTreeMap::new(),
            property_defs: std::collections::BTreeMap::new(),
            mappings: vec![
                MappingEntry {
                    source: "postgres".into(), physical_type: "employees".into(),
                    class: "Employee".into(), property_map: std::collections::BTreeMap::new(),
                },
                MappingEntry {
                    source: "mongo".into(), physical_type: "employee".into(),
                    class: "Employee".into(), property_map: std::collections::BTreeMap::new(),
                },
            ],
        }).unwrap();

        let registry = SchemaRegistry::new();
        let plans = compile_with_ontology(
            "MATCH Employee RETURN *", "test-user", &registry, Some(&ont),
        ).unwrap();
        assert_eq!(plans.len(), 2);
        // Each plan should scan a different physical type.
        let types: Vec<String> = plans.iter().map(|p| {
            match &p.operators[0] {
                IrOp::Scan { type_name, .. } => type_name.clone(),
                _ => panic!("expected Scan"),
            }
        }).collect();
        assert!(types.contains(&"employees".to_string()));
        assert!(types.contains(&"employee".to_string()));
    }

    #[test]
    fn ontology_without_mappings_returns_single_plan() {
        use mnemosyne_kernel::knowledge::ontology::*;
        let mut classes = std::collections::BTreeMap::new();
        classes.insert("Employee".into(), ClassDef {
            name: "Employee".into(), parent: None, description: None,
        });
        let ont = OntologyRegistry::new(OntologyDef {
            namespace: "test".into(), version: "1".into(),
            classes,
            relationships: std::collections::BTreeMap::new(),
            property_defs: std::collections::BTreeMap::new(),
            mappings: vec![],
        }).unwrap();

        let registry = SchemaRegistry::new();
        let plans = compile_with_ontology(
            "MATCH Employee RETURN *", "test-user", &registry, Some(&ont),
        ).unwrap();
        assert_eq!(plans.len(), 1);
        match &plans[0].operators[0] {
            IrOp::Scan { type_name, .. } => assert_eq!(type_name, "Employee"),
            _ => panic!("expected Scan"),
        }
    }
}
