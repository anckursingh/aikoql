//! D5: Ontology Discovery — multi-signal ontology proposals from document IR.
//!
//! Consumes `KnowledgeIr` (D4) and produces evidence-backed ontology proposals:
//! classes, properties, and relationships. These proposals feed into the
//! existing `OntologyRegistry` (kernel) for validation and registration.
//!
//! # Architecture
//! - `ClassProposal` — proposed ontology class with parent hint + evidence
//! - `PropertyProposal` — proposed property with value type + evidence
//! - `RelationshipProposal` — proposed relationship with domain/range + evidence
//! - `OntologyProposal` — container for all proposals from one document
//! - `discover_ontology_from_ir()` — KnowledgeIr → OntologyProposal

use crate::ir::{Evidence, KnowledgeIr};

// ---------------------------------------------------------------------------
// Proposal types — evidence-backed ontology candidates
// ---------------------------------------------------------------------------

/// A proposed ontology class, derived from a document entity.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClassProposal {
    /// Proposed class name (e.g. "Organization", "Invoice", "Employee").
    pub name: String,
    /// Suggested parent class for single inheritance.
    pub parent: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// Count of evidence signals supporting this proposal.
    pub signal_count: u32,
    /// Aggregate confidence from all signals (0.0–1.0).
    pub confidence: f32,
    /// Evidence sources that contributed to this proposal.
    pub evidence: Vec<Evidence>,
}

/// A proposed property for an ontology class.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PropertyProposal {
    /// Property name (e.g. "invoice_date", "total_amount").
    pub name: String,
    /// The class this property belongs to.
    pub class_name: String,
    /// aikoql value type: "Text", "Int", "Float", "Bool", "DateTime".
    pub value_type: String,
    /// Whether this property is required.
    pub required: bool,
    /// Aggregate confidence from all signals.
    pub confidence: f32,
    /// Evidence sources.
    pub evidence: Vec<Evidence>,
}

/// A proposed relationship between two ontology classes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelationshipProposal {
    /// Relationship name (e.g. "employed_by", "has_invoice").
    pub name: String,
    /// Source class (domain).
    pub domain: Option<String>,
    /// Target class (range).
    pub range: Option<String>,
    /// Suggested cardinality, if inferrable.
    pub cardinality: Option<String>,
    /// Aggregate confidence.
    pub confidence: f32,
    /// Evidence sources.
    pub evidence: Vec<Evidence>,
}

/// Container for ontology proposals discovered from document IR.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct OntologyProposal {
    pub classes: Vec<ClassProposal>,
    pub properties: Vec<PropertyProposal>,
    pub relationships: Vec<RelationshipProposal>,
    /// Source document identifier.
    pub document_id: Option<String>,
    /// Name of the discovery method.
    pub method: String,
}

impl OntologyProposal {
    /// True when no proposals were generated.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.properties.is_empty() && self.relationships.is_empty()
    }

    /// Total proposal count across all categories.
    pub fn total_proposals(&self) -> usize {
        self.classes.len() + self.properties.len() + self.relationships.len()
    }
}

// ---------------------------------------------------------------------------
// Discovery: KnowledgeIr → OntologyProposal
// ---------------------------------------------------------------------------

/// Discover ontology proposals from document Knowledge IR.
///
/// Strategy (mock):
/// - **Entities** → ClassProposals. Entity type_hint becomes the class name;
///   if no type_hint, the entity name is used as a candidate class.
/// - **Relations** → RelationshipProposals. Each RelationCandidate's predicate
///   becomes the relationship name; subject/object become domain/range.
/// - **Facts** → PropertyProposals. Fact statements mentioning dates suggest
///   DateTime properties; numeric values suggest Int/Float properties.
/// - **Temporal** → PropertyProposals. Temporal assertions suggest DateTime
///   typed properties on the most relevant class.
pub fn discover_ontology_from_ir(ir: &KnowledgeIr) -> OntologyProposal {
    let mut proposal = OntologyProposal {
        document_id: ir.document_id.clone(),
        method: "ir-discovery-mock".into(),
        ..Default::default()
    };

    // ── Entities → Class proposals ──
    for entity in &ir.entities {
        let class_name = entity
            .type_hint
            .clone()
            .unwrap_or_else(|| to_class_name(&entity.name));

        // Check if we already proposed this class; if so, merge evidence.
        if let Some(existing) = proposal.classes.iter_mut().find(|c| c.name == class_name) {
            existing.signal_count += 1;
            existing.confidence = avg_confidence(existing.confidence, entity.confidence);
            existing.evidence.push(entity.evidence.clone());
            // Upgrade parent if this signal has a stronger type_hint.
            if entity.type_hint.is_some() && existing.parent.is_none() {
                existing.parent = infer_parent(&class_name);
            }
        } else {
            proposal.classes.push(ClassProposal {
                name: class_name.clone(),
                parent: infer_parent(&class_name),
                description: Some(format!("Discovered from entity '{}'", entity.name)),
                signal_count: 1,
                confidence: entity.confidence,
                evidence: vec![entity.evidence.clone()],
            });
        }
    }

    // ── Relations → Relationship proposals ──
    for rel in &ir.relations {
        let domain_class = class_name_for_entity(&ir.entities, &rel.subject);
        let range_class = class_name_for_entity(&ir.entities, &rel.object);

        let rel_name = to_snake_case(&rel.predicate);

        // Merge with existing proposal for the same (name, domain, range).
        if let Some(existing) = proposal
            .relationships
            .iter_mut()
            .find(|r| r.name == rel_name && r.domain == domain_class && r.range == range_class)
        {
            existing.confidence = avg_confidence(existing.confidence, rel.confidence);
            existing.evidence.push(rel.evidence.clone());
        } else {
            proposal.relationships.push(RelationshipProposal {
                name: rel_name,
                domain: domain_class,
                range: range_class,
                cardinality: None,
                confidence: rel.confidence,
                evidence: vec![rel.evidence.clone()],
            });
        }
    }

    // ── Facts → Property proposals ──
    for fact in &ir.facts {
        // If the fact statement contains a date-like pattern, propose a DateTime property.
        if has_date_pattern(&fact.statement) {
            let class_name = fact
                .entities
                .first()
                .map(|e| class_name_for_entity(&ir.entities, e))
                .unwrap_or_else(|| Some("Document".into()))
                .unwrap_or_else(|| "Document".into());

            let prop_name = infer_date_property_name(&fact.statement);
            upsert_property(
                &mut proposal.properties,
                &prop_name,
                &class_name,
                "DateTime",
                false,
                fact.confidence,
                vec![fact.evidence.clone()],
            );
        }

        // If the statement mentions a numeric value, propose Int/Float property.
        if has_numeric_pattern(&fact.statement) {
            let class_name = fact
                .entities
                .first()
                .map(|e| class_name_for_entity(&ir.entities, e))
                .unwrap_or_else(|| Some("Document".into()))
                .unwrap_or_else(|| "Document".into());

            let (prop_name, value_type) = infer_numeric_property(&fact.statement);
            upsert_property(
                &mut proposal.properties,
                &prop_name,
                &class_name,
                &value_type,
                false,
                fact.confidence,
                vec![fact.evidence.clone()],
            );
        }
    }

    // ── Temporal → Property proposals ──
    for temp in &ir.temporal {
        // Each temporal assertion suggests date-related properties.
        let class_name = guess_class_from_temporal(&ir.entities, &temp.text);
        upsert_property(
            &mut proposal.properties,
            &infer_date_property_name(&temp.text),
            &class_name,
            "DateTime",
            false,
            temp.confidence,
            vec![temp.evidence.clone()],
        );
    }

    proposal
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an entity name to a PascalCase class name.
fn to_class_name(entity_name: &str) -> String {
    entity_name
        .split_whitespace()
        .map(|w| {
            let mut chars: Vec<char> = w.chars().collect();
            if let Some(first) = chars.first_mut() {
                *first = first.to_uppercase().next().unwrap_or(*first);
            }
            chars.into_iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Find the class name for an entity reference, using type_hint if available.
fn class_name_for_entity(entities: &[crate::EntityCandidate], entity_name: &str) -> Option<String> {
    entities
        .iter()
        .find(|e| e.name == entity_name)
        .and_then(|e| e.type_hint.clone())
        .or_else(|| Some(to_class_name(entity_name)))
}

/// Infer a parent class from the class name.
fn infer_parent(class_name: &str) -> Option<String> {
    let lower = class_name.to_lowercase();
    if lower.contains("invoice") || lower.contains("receipt") || lower.contains("purchase") {
        Some("FinancialDocument".into())
    } else if lower.contains("employee") || lower.contains("person") || lower.contains("customer") {
        Some("Person".into())
    } else if lower.contains("company") || lower.contains("organization") || lower.contains("corp")
    {
        Some("Organization".into())
    } else if lower.contains("report") || lower.contains("document") {
        Some("Document".into())
    } else {
        None
    }
}

/// Check if text contains a date-like pattern.
fn has_date_pattern(text: &str) -> bool {
    let months = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
        "Jan",
        "Feb",
        "Mar",
        "Apr",
        "Jun",
        "Jul",
        "Aug",
        "Sep",
        "Oct",
        "Nov",
        "Dec",
    ];
    let lower = text.to_lowercase();
    months.iter().any(|m| lower.contains(&m.to_lowercase()))
        || text.contains("date")
        || text.contains("Date")
        || lower.contains("year")
        || lower.contains("fiscal")
        || lower.contains("quarter")
        || lower.contains("q1")
        || lower.contains("q2")
        || lower.contains("q3")
        || lower.contains("q4")
}

/// Check if text contains a numeric value.
fn has_numeric_pattern(text: &str) -> bool {
    text.split_whitespace().any(|w| {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.')
            .parse::<f64>()
            .is_ok()
    })
}

/// Infer a date-related property name from statement text.
fn infer_date_property_name(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.contains("invoice") && lower.contains("date") {
        "invoice_date".into()
    } else if lower.contains("due") && lower.contains("date") {
        "due_date".into()
    } else if lower.contains("fiscal") && lower.contains("year") {
        "fiscal_year".into()
    } else if lower.contains("report") && lower.contains("date") {
        "report_date".into()
    } else if lower.contains("sign") || lower.contains("signed") {
        "signature_date".into()
    } else if lower.contains("effective") {
        "effective_date".into()
    } else if lower.contains("quarter")
        || lower.contains("q1")
        || lower.contains("q2")
        || lower.contains("q3")
        || lower.contains("q4")
    {
        "fiscal_quarter".into()
    } else if lower.contains("year") {
        "year".into()
    } else {
        "date".into()
    }
}

/// Infer a numeric property name and type from statement text.
fn infer_numeric_property(text: &str) -> (String, String) {
    let lower = text.to_lowercase();
    if lower.contains("revenue") || lower.contains("income") {
        ("revenue".into(), "Float".into())
    } else if lower.contains("amount") || lower.contains("total") {
        ("total_amount".into(), "Float".into())
    } else if lower.contains("count") || lower.contains("number of") {
        ("count".into(), "Int".into())
    } else if lower.contains("price") || lower.contains("cost") {
        ("price".into(), "Float".into())
    } else if lower.contains("tax") {
        ("tax_amount".into(), "Float".into())
    } else if lower.contains("rate") || lower.contains("percentage") {
        ("rate".into(), "Float".into())
    } else {
        ("numeric_value".into(), "Float".into())
    }
}

/// Guess which class a temporal assertion belongs to.
fn guess_class_from_temporal(entities: &[crate::EntityCandidate], _text: &str) -> String {
    // Default to the first entity's class, or "Document".
    entities
        .first()
        .and_then(|e| e.type_hint.clone())
        .unwrap_or_else(|| "Document".into())
}

/// Insert or merge a property proposal.
fn upsert_property(
    props: &mut Vec<PropertyProposal>,
    name: &str,
    class_name: &str,
    value_type: &str,
    required: bool,
    confidence: f32,
    evidence: Vec<Evidence>,
) {
    if let Some(existing) = props
        .iter_mut()
        .find(|p| p.name == name && p.class_name == class_name)
    {
        existing.confidence = avg_confidence(existing.confidence, confidence);
        if !required {
            existing.required = required;
        }
        existing.evidence.extend(evidence);
    } else {
        props.push(PropertyProposal {
            name: name.into(),
            class_name: class_name.into(),
            value_type: value_type.into(),
            required,
            confidence,
            evidence,
        });
    }
}

/// Convert a CamelCase or space-separated name to snake_case.
fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch == ' ' || ch == '-' {
            out.push('_');
        } else if ch.is_uppercase() && i > 0 {
            out.push('_');
            out.push(ch.to_lowercase().next().unwrap_or(ch));
        } else {
            out.push(ch.to_lowercase().next().unwrap_or(ch));
        }
    }
    out.trim_matches('_').to_string()
}

/// Average two confidence values (simple mean).
fn avg_confidence(a: f32, b: f32) -> f32 {
    (a + b) / 2.0
}

// ---------------------------------------------------------------------------
// Multi-signal aggregation
// ---------------------------------------------------------------------------

/// Merge multiple `OntologyProposal`s from different signals into one.
///
/// Signals can come from: document IR, DB schemas, existing KOs, etc.
/// Later phases (D6-D7) resolve conflicts and commit the merged ontology.
pub fn merge_proposals(proposals: &[OntologyProposal]) -> OntologyProposal {
    let mut merged = OntologyProposal {
        method: "merged".into(),
        ..Default::default()
    };

    for p in proposals {
        merged.document_id = p.document_id.clone().or(merged.document_id);

        for cp in &p.classes {
            if let Some(existing) = merged.classes.iter_mut().find(|c| c.name == cp.name) {
                existing.signal_count += cp.signal_count;
                existing.confidence = avg_confidence(existing.confidence, cp.confidence);
                existing.evidence.extend(cp.evidence.clone());
                if cp.parent.is_some() && existing.parent.is_none() {
                    existing.parent = cp.parent.clone();
                }
            } else {
                merged.classes.push(cp.clone());
            }
        }

        for pp in &p.properties {
            if let Some(existing) = merged
                .properties
                .iter_mut()
                .find(|p| p.name == pp.name && p.class_name == pp.class_name)
            {
                existing.confidence = avg_confidence(existing.confidence, pp.confidence);
                existing.evidence.extend(pp.evidence.clone());
            } else {
                merged.properties.push(pp.clone());
            }
        }

        for rp in &p.relationships {
            if let Some(existing) = merged
                .relationships
                .iter_mut()
                .find(|r| r.name == rp.name && r.domain == rp.domain && r.range == rp.range)
            {
                existing.confidence = avg_confidence(existing.confidence, rp.confidence);
                existing.evidence.extend(rp.evidence.clone());
            } else {
                merged.relationships.push(rp.clone());
            }
        }
    }

    merged
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate, TemporalAssertion,
    };

    fn evidence(page: u32) -> Evidence {
        Evidence {
            document_id: Some("test-doc.pdf".into()),
            page: Some(page),
            bbox_text: None,
            extractor: "mock".into(),
            model: Some("mock-v1".into()),
            confidence: 0.85,
        }
    }

    fn make_ir_with_entities(entities: Vec<EntityCandidate>) -> KnowledgeIr {
        KnowledgeIr {
            entities,
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        }
    }

    // ── Class proposals from entities ──

    #[test]
    fn entity_with_type_hint_becomes_class() {
        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "Acme Corporation".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corporation".into()],
            confidence: 0.9,
            evidence: evidence(1),
        }]);

        let proposal = discover_ontology_from_ir(&ir);

        assert_eq!(proposal.classes.len(), 1);
        assert_eq!(proposal.classes[0].name, "Organization");
        assert_eq!(proposal.classes[0].signal_count, 1);
        assert!((proposal.classes[0].confidence - 0.9).abs() < 0.001);
    }

    #[test]
    fn entity_without_type_hint_uses_name_as_class() {
        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "Invoice Processor".into(),
            type_hint: None,
            mentions: vec!["Invoice Processor".into()],
            confidence: 0.8,
            evidence: evidence(1),
        }]);

        let proposal = discover_ontology_from_ir(&ir);

        assert_eq!(proposal.classes[0].name, "InvoiceProcessor");
    }

    #[test]
    fn duplicate_entities_merge_into_single_class_proposal() {
        let ir = make_ir_with_entities(vec![
            EntityCandidate {
                name: "Acme Corporation".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corporation".into()],
                confidence: 0.9,
                evidence: evidence(1),
            },
            EntityCandidate {
                name: "Globex Industries".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Globex Industries".into()],
                confidence: 0.7,
                evidence: evidence(2),
            },
        ]);

        let proposal = discover_ontology_from_ir(&ir);

        // Both entities map to "Organization" class → merged.
        let org = proposal
            .classes
            .iter()
            .find(|c| c.name == "Organization")
            .unwrap();
        assert_eq!(org.signal_count, 2);
        assert_eq!(org.evidence.len(), 2);
    }

    #[test]
    fn class_proposal_infers_parent() {
        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "Invoice #12345".into(),
            type_hint: Some("Invoice".into()),
            mentions: vec!["Invoice #12345".into()],
            confidence: 0.85,
            evidence: evidence(1),
        }]);

        let proposal = discover_ontology_from_ir(&ir);

        let invoice = proposal
            .classes
            .iter()
            .find(|c| c.name == "Invoice")
            .unwrap();
        assert_eq!(invoice.parent.as_deref(), Some("FinancialDocument"));
    }

    #[test]
    fn class_proposal_infers_person_parent() {
        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "John Smith".into(),
            type_hint: Some("Employee".into()),
            mentions: vec!["John Smith".into()],
            confidence: 0.8,
            evidence: evidence(1),
        }]);

        let proposal = discover_ontology_from_ir(&ir);

        let emp = proposal
            .classes
            .iter()
            .find(|c| c.name == "Employee")
            .unwrap();
        assert_eq!(emp.parent.as_deref(), Some("Person"));
    }

    // ── Relationship proposals ──

    #[test]
    fn relations_become_relationship_proposals() {
        let ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "Acme Corporation".into(),
                    type_hint: Some("Organization".into()),
                    mentions: vec!["Acme Corporation".into()],
                    confidence: 0.9,
                    evidence: evidence(1),
                },
                EntityCandidate {
                    name: "John Smith".into(),
                    type_hint: Some("Person".into()),
                    mentions: vec!["John Smith".into()],
                    confidence: 0.8,
                    evidence: evidence(1),
                },
            ],
            relations: vec![RelationCandidate {
                subject: "John Smith".into(),
                predicate: "employed_by".into(),
                object: "Acme Corporation".into(),
                confidence: 0.75,
                evidence: evidence(1),
            }],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);

        assert_eq!(proposal.relationships.len(), 1);
        let rel = &proposal.relationships[0];
        assert_eq!(rel.name, "employed_by");
        assert_eq!(rel.domain.as_deref(), Some("Person"));
        assert_eq!(rel.range.as_deref(), Some("Organization"));
    }

    #[test]
    fn duplicate_relations_merge() {
        let ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "Acme Corp".into(),
                    type_hint: Some("Organization".into()),
                    mentions: vec!["Acme Corp".into()],
                    confidence: 0.9,
                    evidence: evidence(1),
                },
                EntityCandidate {
                    name: "Invoice #1".into(),
                    type_hint: Some("Invoice".into()),
                    mentions: vec!["Invoice #1".into()],
                    confidence: 0.8,
                    evidence: evidence(1),
                },
            ],
            relations: vec![
                RelationCandidate {
                    subject: "Acme Corp".into(),
                    predicate: "has_invoice".into(),
                    object: "Invoice #1".into(),
                    confidence: 0.9,
                    evidence: evidence(1),
                },
                RelationCandidate {
                    subject: "Acme Corp".into(),
                    predicate: "has_invoice".into(),
                    object: "Invoice #1".into(),
                    confidence: 0.7,
                    evidence: evidence(2),
                },
            ],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);

        assert_eq!(proposal.relationships.len(), 1);
        let rel = &proposal.relationships[0];
        assert_eq!(rel.evidence.len(), 2);
    }

    // ── Property proposals from facts ──

    #[test]
    fn date_fact_proposes_datetime_property() {
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Invoice #1".into(),
                type_hint: Some("Invoice".into()),
                mentions: vec!["Invoice #1".into()],
                confidence: 0.85,
                evidence: evidence(1),
            }],
            facts: vec![FactCandidate {
                statement: "Invoice date is January 2024".into(),
                entities: vec!["Invoice #1".into()],
                confidence: 0.8,
                evidence: evidence(1),
            }],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);

        let date_prop = proposal
            .properties
            .iter()
            .find(|p| p.name == "invoice_date")
            .unwrap();
        assert_eq!(date_prop.value_type, "DateTime");
        assert_eq!(date_prop.class_name, "Invoice");
    }

    #[test]
    fn revenue_fact_proposes_float_property() {
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Acme Corp".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corp".into()],
                confidence: 0.85,
                evidence: evidence(1),
            }],
            facts: vec![FactCandidate {
                statement: "Acme Corporation Reports Record Revenue of 50 million".into(),
                entities: vec!["Acme Corp".into()],
                confidence: 0.8,
                evidence: evidence(1),
            }],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);

        let rev_prop = proposal
            .properties
            .iter()
            .find(|p| p.name == "revenue")
            .unwrap();
        assert_eq!(rev_prop.value_type, "Float");
    }

    // ── Property proposals from temporal ──

    #[test]
    fn temporal_assertion_proposes_datetime_property() {
        let ir = KnowledgeIr {
            entities: vec![EntityCandidate {
                name: "Acme Corp".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corp".into()],
                confidence: 0.85,
                evidence: evidence(1),
            }],
            temporal: vec![TemporalAssertion {
                text: "January 2024".into(),
                start_time: Some("2024-01-01T00:00:00Z".into()),
                end_time: Some("2024-01-31T23:59:59Z".into()),
                confidence: 0.9,
                evidence: evidence(1),
            }],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);

        assert!(!proposal.properties.is_empty());
        let date_prop = &proposal.properties[0];
        assert_eq!(date_prop.value_type, "DateTime");
        assert_eq!(date_prop.class_name, "Organization");
    }

    // ── OntologyProposal container ──

    #[test]
    fn empty_ir_produces_empty_proposal() {
        let ir = KnowledgeIr::default();
        let proposal = discover_ontology_from_ir(&ir);
        assert!(proposal.is_empty());
        assert_eq!(proposal.total_proposals(), 0);
    }

    #[test]
    fn total_proposals_counts_all_types() {
        // Create an IR that produces classes, properties, and relationships.
        let ir = KnowledgeIr {
            entities: vec![
                EntityCandidate {
                    name: "Acme Corp".into(),
                    type_hint: Some("Organization".into()),
                    mentions: vec!["Acme Corp".into()],
                    confidence: 0.9,
                    evidence: evidence(1),
                },
                EntityCandidate {
                    name: "John Smith".into(),
                    type_hint: Some("Person".into()),
                    mentions: vec!["John Smith".into()],
                    confidence: 0.8,
                    evidence: evidence(1),
                },
            ],
            relations: vec![RelationCandidate {
                subject: "John Smith".into(),
                predicate: "employed_by".into(),
                object: "Acme Corp".into(),
                confidence: 0.75,
                evidence: evidence(1),
            }],
            facts: vec![FactCandidate {
                statement: "Invoice date January 2024".into(),
                entities: vec!["Invoice #1".into()],
                confidence: 0.8,
                evidence: evidence(1),
            }],
            document_id: Some("test-doc.pdf".into()),
            page_count: 1,
            extractor: "mock".into(),
            ..Default::default()
        };

        let proposal = discover_ontology_from_ir(&ir);
        assert!(proposal.total_proposals() >= 3); // 2 classes + 1 relationship + at least 1 property
    }

    // ── Merge proposals ──

    #[test]
    fn merge_combines_signals_from_multiple_sources() {
        let p1 = OntologyProposal {
            classes: vec![ClassProposal {
                name: "Organization".into(),
                parent: None,
                description: Some("From IR".into()),
                signal_count: 2,
                confidence: 0.9,
                evidence: vec![evidence(1)],
            }],
            method: "ir-discovery".into(),
            ..Default::default()
        };

        let p2 = OntologyProposal {
            classes: vec![ClassProposal {
                name: "Organization".into(),
                parent: Some("LegalEntity".into()),
                description: Some("From DB schema".into()),
                signal_count: 3,
                confidence: 0.95,
                evidence: vec![evidence(2)],
            }],
            method: "db-discovery".into(),
            ..Default::default()
        };

        let merged = merge_proposals(&[p1, p2]);

        assert_eq!(merged.classes.len(), 1);
        let org = &merged.classes[0];
        assert_eq!(org.signal_count, 5); // 2 + 3
        assert_eq!(org.evidence.len(), 2);
        // Parent from p2 wins (p1 had None).
        assert_eq!(org.parent.as_deref(), Some("LegalEntity"));
    }

    #[test]
    fn merge_preserves_all_unique_classes() {
        let p1 = OntologyProposal {
            classes: vec![ClassProposal {
                name: "Organization".into(),
                parent: None,
                description: None,
                signal_count: 1,
                confidence: 0.9,
                evidence: vec![evidence(1)],
            }],
            method: "ir".into(),
            ..Default::default()
        };

        let p2 = OntologyProposal {
            classes: vec![ClassProposal {
                name: "Invoice".into(),
                parent: Some("FinancialDocument".into()),
                description: None,
                signal_count: 1,
                confidence: 0.8,
                evidence: vec![evidence(2)],
            }],
            method: "db".into(),
            ..Default::default()
        };

        let merged = merge_proposals(&[p1, p2]);
        assert_eq!(merged.classes.len(), 2);
    }

    // ── Edge cases ──

    #[test]
    fn handles_ir_with_mixed_entity_types() {
        let ir = make_ir_with_entities(vec![
            EntityCandidate {
                name: "Acme Corporation".into(),
                type_hint: Some("Organization".into()),
                mentions: vec!["Acme Corporation".into()],
                confidence: 0.9,
                evidence: evidence(1),
            },
            EntityCandidate {
                name: "John Smith".into(),
                type_hint: Some("Person".into()),
                mentions: vec!["John Smith".into()],
                confidence: 0.8,
                evidence: evidence(1),
            },
            EntityCandidate {
                name: "New York".into(),
                type_hint: Some("Location".into()),
                mentions: vec!["New York".into()],
                confidence: 0.7,
                evidence: evidence(1),
            },
        ]);

        let proposal = discover_ontology_from_ir(&ir);

        let class_names: Vec<&str> = proposal.classes.iter().map(|c| c.name.as_str()).collect();
        assert!(class_names.contains(&"Organization"));
        assert!(class_names.contains(&"Person"));
        assert!(class_names.contains(&"Location"));
    }

    #[test]
    fn document_id_propagates_to_proposal() {
        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "Test".into(),
            type_hint: Some("TestClass".into()),
            mentions: vec!["Test".into()],
            confidence: 0.5,
            evidence: Evidence {
                document_id: Some("my-doc.pdf".into()),
                ..evidence(1)
            },
        }]);

        let mut ir_with_doc_id = ir;
        ir_with_doc_id.document_id = Some("my-doc.pdf".into());

        let proposal = discover_ontology_from_ir(&ir_with_doc_id);
        assert_eq!(proposal.document_id.as_deref(), Some("my-doc.pdf"));
    }

    #[test]
    fn evidence_preserved_in_class_proposal() {
        let ev = Evidence {
            document_id: Some("doc.pdf".into()),
            page: Some(3),
            bbox_text: Some("(100,200,300,40)".into()),
            extractor: "mock".into(),
            model: Some("mock-v1".into()),
            confidence: 0.85,
        };

        let ir = make_ir_with_entities(vec![EntityCandidate {
            name: "Acme Corp".into(),
            type_hint: Some("Organization".into()),
            mentions: vec!["Acme Corp".into()],
            confidence: 0.85,
            evidence: ev.clone(),
        }]);

        let proposal = discover_ontology_from_ir(&ir);
        let class_ev = &proposal.classes[0].evidence[0];
        assert_eq!(class_ev.page, Some(3));
        assert_eq!(class_ev.bbox_text.as_deref(), Some("(100,200,300,40)"));
    }
}
