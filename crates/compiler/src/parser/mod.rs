//! Aikoql Text Parser — Lexer → AST → KIR per MRFC-0010.
//!
//! Entry point: `compile(source)` — tokenizes, parses, and compiles to `IrPlan`.

pub mod ast;
pub mod diagnostics;
pub mod lexer;
pub mod parser;

use aikoql_kernel::ir::*;
use aikoql_kernel::knowledge::ontology::OntologyRegistry;
use aikoql_kernel::lifecycle::schema::SchemaRegistry;
use aikoql_kernel::Value;
use parser::Parser;

/// Parse aikoql source into an AST (without compiling to IR).
/// Useful for statement-type dispatch (CREATE executes directly, MATCH compiles to IR).
pub fn parse(source: &str) -> Result<ast::Statement, String> {
    let mut p = Parser::new(source);
    p.parse_statement().map_err(|e| e.to_string())
}

/// Caller identity carried into the plan's Scan operator (R9).
#[derive(Clone, Debug, Default)]
struct ScanSubject {
    name: String,
    roles: Vec<String>,
    tenant: Option<String>,
}

impl From<&str> for ScanSubject {
    fn from(name: &str) -> Self {
        ScanSubject {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// Compile aikoql source text into a validated `IrPlan`.
pub fn compile(source: &str) -> Result<IrPlan, String> {
    compile_with_subject(source, "query-user")
}

/// Compile with an explicit subject for ACL resolution.
pub fn compile_with_subject(source: &str, subject: &str) -> Result<IrPlan, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    ast_to_ir(&stmt, &subject.into())
}

/// Compile with the full caller identity — subject name, roles, and tenant
/// scope (R9). The Scan operator carries all three so runtime ACL evaluation
/// sees the caller's roles and tenant confinement.
pub fn compile_scoped(
    source: &str,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
) -> Result<IrPlan, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    ast_to_ir(
        &stmt,
        &ScanSubject {
            name: subject.into(),
            roles: roles.to_vec(),
            tenant: tenant.map(String::from),
        },
    )
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
    ast_to_ir(&stmt, &subject.into())
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
    compile_with_ontology_scoped(source, subject, &[], None, registry, ontology)
}

/// `compile_with_ontology` with the full caller identity (R9) — roles and
/// tenant scope ride on the emitted Scan operators.
pub fn compile_with_ontology_scoped(
    source: &str,
    subject: &str,
    roles: &[String],
    tenant: Option<&str>,
    registry: &SchemaRegistry,
    ontology: Option<&OntologyRegistry>,
) -> Result<Vec<IrPlan>, String> {
    let mut p = Parser::new(source);
    let stmt = p.parse_statement().map_err(|e| e.to_string())?;
    crate::semantic::SemanticAnalyzer::new(registry)
        .with_ontology(ontology.unwrap_or(&OntologyRegistry::empty()))
        .analyze(&stmt)
        .map_err(|e| e.to_string())?;

    let subj = ScanSubject {
        name: subject.into(),
        roles: roles.to_vec(),
        tenant: tenant.map(String::from),
    };
    let plan = ast_to_ir(&stmt, &subj)?;

    // Ontology expansion: if the Scan entity is an ontology class with
    // physical mappings, clone one plan per mapping, substituting the
    // physical type_name for the Scan operator.
    match (&stmt, ontology) {
        (ast::Statement::Match(m), Some(ont)) => {
            let pts = ont.physical_types_for_class(&m.entity);
            if pts.is_empty() {
                Ok(vec![plan])
            } else {
                pts.into_iter()
                    .map(|(_source, physical_type)| {
                        let mut plan = plan.clone();
                        // Replace the Scan's type_name with the physical type.
                        for op in &mut plan.operators {
                            if let IrOp::Scan {
                                ref mut type_name, ..
                            } = op
                            {
                                *type_name = physical_type.clone();
                                break;
                            }
                        }
                        plan.description =
                            Some(format!("MATCH {} (mapped to {})", m.entity, physical_type));
                        Ok(plan)
                    })
                    .collect()
            }
        }
        _ => Ok(vec![plan]),
    }
}

fn ast_to_ir(stmt: &ast::Statement, subject: &ScanSubject) -> Result<IrPlan, String> {
    match stmt {
        ast::Statement::Match(m) => compile_match(m, subject),
        ast::Statement::Create(c) => compile_create(c, subject),
        ast::Statement::Update(u) => compile_update(u, subject),
        ast::Statement::Delete(d) => compile_delete(d, subject),
        ast::Statement::Ingest(_) => Err("INGEST not yet supported in KIR".into()),
    }
}

fn scan_op(type_name: &str, subject: &ScanSubject) -> IrOp {
    IrOp::Scan {
        type_name: type_name.into(),
        subject: subject.name.clone(),
        roles: subject.roles.clone(),
        tenant: subject.tenant.clone(),
    }
}

fn compile_create(c: &ast::CreateStatement, subject: &ScanSubject) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![scan_op(&c.entity, subject)])
        .with_description(format!("CREATE {}", c.entity));
    Ok(plan)
}

fn compile_update(u: &ast::UpdateStatement, subject: &ScanSubject) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![scan_op(&u.entity, subject)])
        .with_description(format!("UPDATE {} {}", u.entity, u.koid));
    Ok(plan)
}

fn compile_delete(d: &ast::DeleteStatement, subject: &ScanSubject) -> Result<IrPlan, String> {
    let plan = IrPlan::new(vec![scan_op(&d.entity, subject)])
        .with_description(format!("DELETE {} {}", d.entity, d.koid));
    Ok(plan)
}

fn compile_match(m: &ast::MatchStatement, subject: &ScanSubject) -> Result<IrPlan, String> {
    let mut ops = Vec::new();

    // Scan.
    ops.push(scan_op(&m.entity, subject));

    // v0.3 K2: temporal + epistemic operators run right after Scan, before
    // Filter — AS_OF/HISTORICAL reconstruct versions, and the property
    // predicates must apply to the reconstructed rows, not the heads.
    match &m.temporal {
        Some(ast::TemporalClause::AsOf(at)) => ops.push(IrOp::Temporal {
            op: TemporalOp::AsOf(*at),
        }),
        Some(ast::TemporalClause::Between { from, to }) => ops.push(IrOp::Temporal {
            op: TemporalOp::Between {
                from: *from,
                to: *to,
            },
        }),
        Some(ast::TemporalClause::Historical) => ops.push(IrOp::Temporal {
            op: TemporalOp::Historical,
        }),
        None => {}
    }
    if let Some(ref ep) = m.epistemic {
        ops.push(IrOp::EpistemicFilter {
            allowed: ep.allowed.clone(),
        });
    }
    if let Some(ref src) = m.provenance {
        ops.push(IrOp::ProvenanceFilter {
            source: src.clone(),
        });
    }

    // Predicates → Filter.
    let flat = flatten_predicates(&m.predicates);
    if !flat.is_empty() {
        ops.push(IrOp::Filter { predicates: flat });
    }

    // H2 strategy choice: temporal/epistemic queries are answered
    // relationally — no vector search, no fusion. SIMILAR text still
    // applies as TextSearch (BM25 when requested).
    let relational_first = m.temporal.is_some() || m.epistemic.is_some();

    // SIMILAR → TextSearch (default Jaccard), optionally with BM25 scoring.
    // USING EMBEDDING → AnnSearch (vector ANN).
    // Both → TextSearch + AnnSearch + Fuse (hybrid retrieval).
    if let Some(ref sim) = m.similarity {
        let use_bm25 = matches!(sim.score, Some(ast::ScoringMethod::Bm25));
        let use_embedding =
            matches!(sim.using, Some(ast::UsingMethod::Embedding)) && !relational_first;

        if use_embedding {
            ops.push(IrOp::AnnSearch {
                vector: Vec::new(),
                query_text: Some(sim.query.clone()),
                embedding_model: None,
                k: 10,
            });
        }
        ops.push(IrOp::TextSearch {
            query: sim.query.clone(),
            k: 10,
            scoring: if use_bm25 { Some("bm25".into()) } else { None },
        });
        if use_embedding && use_bm25 {
            ops.push(IrOp::Fuse {
                mode: FuseMode::Rrf { k0: 60 },
            });
        } else if use_embedding {
            // Vector + default text: fuse with RRF.
            ops.push(IrOp::Fuse {
                mode: FuseMode::Rrf { k0: 60 },
            });
        }
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
            ast::Projection::Star => {}    // no Project needed — return all fields
            ast::Projection::Explain => {} // handled by explain_endpoint separately
            ast::Projection::Fields(fields) => {
                ops.push(IrOp::Project {
                    fields: fields.clone(),
                });
            }
        }
    }

    // EXE-006: LIMIT/OFFSET applies to the final deterministic row order —
    // last operator in the pipeline (after Project/Traverse).
    if let Some(limit) = m.limit {
        ops.push(IrOp::Limit {
            limit,
            offset: m.offset.unwrap_or(0),
        });
    }

    let plan = IrPlan::new(ops).with_description(if relational_first {
        format!(
            "MATCH {} — strategy: relational (vector search skipped)",
            m.entity
        )
    } else {
        format!("MATCH {}", m.entity)
    });
    plan.validate()
        .map_err(|e| format!("AIKOQL1014: conflicting clauses — {}", e))?;
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
                out.extend(flatten_predicates(&[
                    left.as_ref().clone(),
                    right.as_ref().clone(),
                ]));
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
            IrOp::Traverse {
                start_koid,
                rel_type,
                depth,
            } => {
                assert!(
                    start_koid.is_empty(),
                    "set-based Traverse uses empty start_koid"
                );
                assert_eq!(rel_type.as_deref(), Some("knows"));
                assert_eq!(*depth, 1);
            }
            _ => panic!("expected Traverse as second op"),
        }
    }

    #[test]
    fn compile_match_as_of_lowers_to_temporal_op() {
        let plan = compile("MATCH Person AS_OF 1735689600000 RETURN *").unwrap();
        // Scan + Temporal(AsOf)
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[1] {
            IrOp::Temporal { op } => assert_eq!(*op, TemporalOp::AsOf(1_735_689_600_000)),
            _ => panic!("expected Temporal op"),
        }
    }

    #[test]
    fn compile_match_between_with_iso_strings() {
        let plan =
            compile(r#"MATCH Person BETWEEN "2026-01-01" AND "2026-02-01" RETURN *"#).unwrap();
        match &plan.operators[1] {
            IrOp::Temporal { op } => assert_eq!(
                *op,
                TemporalOp::Between {
                    from: 1_767_225_600_000, // 2026-01-01T00:00:00Z
                    to: 1_769_904_000_000,   // 2026-02-01T00:00:00Z
                }
            ),
            _ => panic!("expected Temporal op"),
        }
    }

    #[test]
    fn compile_match_historical_and_epistemic() {
        let plan = compile("MATCH Fact HISTORICAL EPISTEMIC verified, asserted RETURN *").unwrap();
        // Scan + Temporal(Historical) + EpistemicFilter
        assert_eq!(plan.operators.len(), 3);
        assert!(matches!(
            &plan.operators[1],
            IrOp::Temporal {
                op: TemporalOp::Historical
            }
        ));
        assert!(matches!(&plan.operators[2], IrOp::EpistemicFilter { .. }));
        assert!(plan.description.as_deref().unwrap().contains("relational"));
    }

    #[test]
    fn temporal_query_skips_vector_search_h2() {
        // H2: temporal queries are relational — USING EMBEDDING degrades to
        // text search; no AnnSearch, no Fuse.
        let plan =
            compile("MATCH Fact SIMILAR TO \"messaging\" USING EMBEDDING AS_OF 1000 RETURN *")
                .unwrap();
        for op in &plan.operators {
            assert!(
                !matches!(op, IrOp::AnnSearch { .. } | IrOp::Fuse { .. }),
                "temporal query must not plan vector search: {:?}",
                op
            );
        }
        assert!(plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::TextSearch { .. })));
        assert!(plan
            .description
            .as_deref()
            .unwrap()
            .contains("vector search skipped"));
    }

    #[test]
    fn temporal_clause_placed_before_filter() {
        // WHERE must apply to reconstructed versions, not heads — the
        // temporal op lands before Filter in the operator pipeline.
        let plan = compile("MATCH Fact WHERE severity == \"high\" AS_OF 1000 RETURN *").unwrap();
        assert!(matches!(plan.operators[1], IrOp::Temporal { .. }));
        assert!(matches!(plan.operators[2], IrOp::Filter { .. }));
    }

    #[test]
    fn compile_match_source_lowers_to_provenance_filter() {
        let plan = compile("MATCH Fact SOURCE \"sec-filing\" RETURN *").unwrap();
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[1] {
            IrOp::ProvenanceFilter { source } => assert_eq!(source, "sec-filing"),
            _ => panic!("expected ProvenanceFilter op"),
        }
    }

    #[test]
    fn provenance_clause_placed_before_filter() {
        // WHERE must apply after provenance retention — the filter op lands
        // after ProvenanceFilter in the operator pipeline.
        let plan = compile("MATCH Fact SOURCE \"sec-filing\" WHERE severity == \"high\" RETURN *")
            .unwrap();
        assert!(matches!(plan.operators[1], IrOp::ProvenanceFilter { .. }));
        assert!(matches!(plan.operators[2], IrOp::Filter { .. }));
    }

    #[test]
    fn compile_match_limit_lowers_to_limit_op() {
        let plan = compile("MATCH Fact LIMIT 3 OFFSET 1 RETURN *").unwrap();
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[1] {
            IrOp::Limit { limit, offset } => {
                assert_eq!(*limit, 3);
                assert_eq!(*offset, 1);
            }
            _ => panic!("expected Limit op"),
        }
    }

    #[test]
    fn compile_match_limit_lands_after_project() {
        // Pagination is the last operator — it trims the final row order,
        // never the intermediate rowset.
        let plan = compile("MATCH Fact LIMIT 3 RETURN koid").unwrap();
        assert!(matches!(plan.operators[1], IrOp::Project { .. }));
        assert!(matches!(plan.operators[2], IrOp::Limit { .. }));
    }

    #[test]
    fn ontology_expands_to_physical_scans() {
        use aikoql_kernel::knowledge::ontology::*;
        let mut classes = std::collections::BTreeMap::new();
        classes.insert(
            "Employee".into(),
            ClassDef {
                name: "Employee".into(),
                parent: None,
                description: None,
            },
        );
        let ont = OntologyRegistry::new(OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships: std::collections::BTreeMap::new(),
            property_defs: std::collections::BTreeMap::new(),
            mappings: vec![
                MappingEntry {
                    source: "postgres".into(),
                    physical_type: "employees".into(),
                    class: "Employee".into(),
                    property_map: std::collections::BTreeMap::new(),
                },
                MappingEntry {
                    source: "mongo".into(),
                    physical_type: "employee".into(),
                    class: "Employee".into(),
                    property_map: std::collections::BTreeMap::new(),
                },
            ],
        })
        .unwrap();

        let registry = SchemaRegistry::new();
        let plans = compile_with_ontology(
            "MATCH Employee RETURN *",
            "test-user",
            &registry,
            Some(&ont),
        )
        .unwrap();
        assert_eq!(plans.len(), 2);
        // Each plan should scan a different physical type.
        let types: Vec<String> = plans
            .iter()
            .map(|p| match &p.operators[0] {
                IrOp::Scan { type_name, .. } => type_name.clone(),
                _ => panic!("expected Scan"),
            })
            .collect();
        assert!(types.contains(&"employees".to_string()));
        assert!(types.contains(&"employee".to_string()));
    }

    #[test]
    fn ontology_without_mappings_returns_single_plan() {
        use aikoql_kernel::knowledge::ontology::*;
        let mut classes = std::collections::BTreeMap::new();
        classes.insert(
            "Employee".into(),
            ClassDef {
                name: "Employee".into(),
                parent: None,
                description: None,
            },
        );
        let ont = OntologyRegistry::new(OntologyDef {
            namespace: "test".into(),
            version: "1".into(),
            classes,
            relationships: std::collections::BTreeMap::new(),
            property_defs: std::collections::BTreeMap::new(),
            mappings: vec![],
        })
        .unwrap();

        let registry = SchemaRegistry::new();
        let plans = compile_with_ontology(
            "MATCH Employee RETURN *",
            "test-user",
            &registry,
            Some(&ont),
        )
        .unwrap();
        assert_eq!(plans.len(), 1);
        match &plans[0].operators[0] {
            IrOp::Scan { type_name, .. } => assert_eq!(type_name, "Employee"),
            _ => panic!("expected Scan"),
        }
    }

    // ---- R13: SCORE BM25 / USING EMBEDDING lowering tests ----

    #[test]
    fn compile_similar_score_bm25() {
        let plan = compile("MATCH Person SIMILAR TO \"John\" SCORE BM25 RETURN *").unwrap();
        // Scan + TextSearch (with scoring=bm25)
        assert_eq!(plan.operators.len(), 2);
        match &plan.operators[1] {
            IrOp::TextSearch { query, scoring, .. } => {
                assert_eq!(query, "John");
                assert_eq!(scoring.as_deref(), Some("bm25"));
            }
            _ => panic!("expected TextSearch"),
        }
    }

    #[test]
    fn compile_similar_using_embedding() {
        let plan = compile("MATCH Doc SIMILAR TO \"concept\" USING EMBEDDING RETURN *").unwrap();
        // Scan + AnnSearch (with query_text) + TextSearch + Fuse
        assert!(plan.operators.len() >= 3);
        let has_ann = plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::AnnSearch { .. }));
        let has_fuse = plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::Fuse { .. }));
        assert!(has_ann, "expected AnnSearch in plan");
        assert!(has_fuse, "expected Fuse in plan");
    }

    #[test]
    fn compile_similar_both_bm25_and_embedding() {
        let plan = compile("MATCH X SIMILAR TO \"q\" SCORE BM25 USING EMBEDDING RETURN *").unwrap();
        // Scan + AnnSearch + TextSearch(bm25) + Fuse
        assert!(plan.operators.len() >= 4);
        let has_ann = plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::AnnSearch { .. }));
        let has_text = plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::TextSearch { .. }));
        let has_fuse = plan
            .operators
            .iter()
            .any(|op| matches!(op, IrOp::Fuse { .. }));
        assert!(has_ann, "expected AnnSearch");
        assert!(has_text, "expected TextSearch");
        assert!(has_fuse, "expected Fuse");
    }

    #[test]
    fn compile_similar_plain_unchanged() {
        // Backward compat: plain SIMILAR TO without SCORE/USING still works.
        let plan = compile("MATCH Person SIMILAR TO \"John\" RETURN *").unwrap();
        assert_eq!(plan.operators.len(), 2); // Scan + TextSearch (no scoring)
        match &plan.operators[1] {
            IrOp::TextSearch { query, scoring, .. } => {
                assert_eq!(query, "John");
                assert_eq!(scoring.as_deref(), None);
            }
            _ => panic!("expected TextSearch"),
        }
    }
}
