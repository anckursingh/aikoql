//! Semantic Analyzer — MRFC-0010 §3.
//!
//! Sits between AST and KIR. Validates entity names, property names, and
//! (when Schema carries types) predicate types against the SchemaRegistry.
//! When an ontology is configured, also validates relationship names and
//! resolves class inheritance.
//!
//! Error codes: AIKOQL1030 (unknown type), AIKOQL1031 (unknown property),
//! AIKOQL1032 (type mismatch), AIKOQL1033 (unknown relationship).

use mnemosyne_kernel::knowledge::ontology::OntologyRegistry;
use mnemosyne_kernel::lifecycle::schema::SchemaRegistry;
use mnemosyne_kernel::knowledge::kom::Schema;

use super::parser::ast::*;
use super::parser::diagnostics::{self, Diagnostic};

pub struct SemanticAnalyzer<'a> {
    registry: &'a SchemaRegistry,
    ontology: Option<&'a OntologyRegistry>,
}

impl<'a> SemanticAnalyzer<'a> {
    pub fn new(registry: &'a SchemaRegistry) -> Self {
        SemanticAnalyzer { registry, ontology: None }
    }

    /// Enable ontology-aware validation (class inheritance, relationship names).
    pub fn with_ontology(mut self, ontology: &'a OntologyRegistry) -> Self {
        self.ontology = Some(ontology);
        self
    }

    /// Validate a parsed statement. Returns the statement unchanged on success
    /// (the analyzer only checks correctness — it doesn't rewrite the AST).
    pub fn analyze(&self, stmt: &Statement) -> Result<(), Diagnostic> {
        match stmt {
            Statement::Match(m) => self.analyze_match(m),
            Statement::Create(c) => self.analyze_create(c),
            Statement::Update(u) => self.analyze_update(u),
            Statement::Delete(d) => self.analyze_delete(d),
            Statement::Ingest(_) => Ok(()), // no entity references to validate
        }
    }

    // ---- per-statement analyzers ----

    fn analyze_match(&self, m: &MatchStatement) -> Result<(), Diagnostic> {
        let schema = self.resolve_entity(&m.entity)?;
        if let Some(schema) = schema {
            for pred in &m.predicates {
                self.check_predicate_properties(pred, schema)?;
            }
        }
        // Validate TRAVERSE relationship against ontology.
        if let Some(ref trav) = m.traverse {
            if let Some(ont) = self.ontology {
                // Resolve the domain: if m.entity is a physical type, look up its ontology class.
                let domain_class = ont.class_for_physical(&m.entity)
                    .unwrap_or(&m.entity);
                if ont.resolve_relationship(domain_class, &trav.relation).is_none() {
                    return Err(diagnostics::unknown_relationship(
                        &trav.relation, &m.entity, 0, 0,
                    ));
                }
            }
        }
        Ok(())
    }

    fn analyze_create(&self, c: &CreateStatement) -> Result<(), Diagnostic> {
        let schema = self.resolve_entity(&c.entity)?;
        if let Some(schema) = schema {
            for (prop, _val) in &c.properties {
                self.check_property(prop, schema)?;
            }
        }
        Ok(())
    }

    fn analyze_update(&self, u: &UpdateStatement) -> Result<(), Diagnostic> {
        let schema = self.resolve_entity(&u.entity)?;
        if let Some(schema) = schema {
            for (prop, _val) in &u.properties {
                self.check_property(prop, schema)?;
            }
        }
        Ok(())
    }

    fn analyze_delete(&self, d: &DeleteStatement) -> Result<(), Diagnostic> {
        self.resolve_entity(&d.entity)?;
        Ok(())
    }

    // ---- helpers ----

    fn resolve_entity(&self, name: &str) -> Result<Option<&Schema>, Diagnostic> {
        // Built-in types always pass validation.
        if name == mnemosyne_kernel::knowledge::ontology::ONTOLOGY_TYPE || name.starts_with("mnemosyne:") {
            return Ok(None);
        }
        // Ontology class or physical mapping takes priority.
        if let Some(ont) = self.ontology {
            if ont.resolve_class(name).is_some() {
                return Ok(None);
            }
            // Also accept any physical type that appears in an ontology mapping.
            // ponytail: O(n) scan; build set if this becomes a hot path.
            for me in ont.definition().mappings.iter() {
                if me.physical_type == name {
                    return Ok(None);
                }
            }
        }
        // Fallback: must be registered in SchemaRegistry.
        self.registry.get(name)
            .map(Some)
            .ok_or_else(|| diagnostics::unknown_type(name, 0, 0))
    }

    fn check_predicate_properties(&self, pred: &Predicate, schema: &Schema) -> Result<(), Diagnostic> {
        match pred {
            Predicate::Eq { property, .. }
            | Predicate::Neq { property, .. }
            | Predicate::Gt { property, .. }
            | Predicate::Lt { property, .. }
            | Predicate::Gte { property, .. }
            | Predicate::Lte { property, .. } => self.check_property(property, schema)?,
            Predicate::And { left, right } | Predicate::Or { left, right } => {
                self.check_predicate_properties(left, schema)?;
                self.check_predicate_properties(right, schema)?;
            }
        }
        Ok(())
    }

    fn check_property(&self, name: &str, schema: &Schema) -> Result<(), Diagnostic> {
        // If schema is closed (allowed_properties is Some), check membership.
        if let Some(ref allowed) = schema.allowed_properties {
            if !allowed.contains(name) {
                return Err(diagnostics::unknown_property(
                    name,
                    &schema.type_name,
                    0,
                    0,
                ));
            }
        }
        // Open-world schemas allow any property — skip.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemosyne_kernel::knowledge::kom::Schema;

    fn registry_with_person() -> SchemaRegistry {
        let mut r = SchemaRegistry::new();
        let mut schema = Schema::new("Person", 1);
        let mut allowed = std::collections::HashSet::new();
        allowed.insert("name".into());
        allowed.insert("company".into());
        allowed.insert("city".into());
        schema.allowed_properties = Some(allowed);
        r.register(schema);
        r
    }

    #[test]
    fn valid_match() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("MATCH Person WHERE company == \"Visa\" RETURN *").unwrap();
        assert!(a.analyze(&stmt).is_ok());
    }

    #[test]
    fn unknown_entity() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("MATCH Bogus RETURN *").unwrap();
        let err = a.analyze(&stmt).unwrap_err();
        assert_eq!(err.code, diagnostics::Code::UnknownType);
        assert!(err.format().starts_with("AIKOQL1030"));
    }

    #[test]
    fn unknown_property() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("MATCH Person WHERE bogus == \"x\" RETURN *").unwrap();
        let err = a.analyze(&stmt).unwrap_err();
        assert_eq!(err.code, diagnostics::Code::UnknownProperty);
        assert!(err.format().starts_with("AIKOQL1031"));
    }

    #[test]
    fn valid_create() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("CREATE Person name == \"Alice\"").unwrap();
        assert!(a.analyze(&stmt).is_ok());
    }

    #[test]
    fn create_unknown_entity() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("CREATE Bogus name == \"x\"").unwrap();
        let err = a.analyze(&stmt).unwrap_err();
        assert_eq!(err.code, diagnostics::Code::UnknownType);
    }

    #[test]
    fn open_world_accepts_any_property() {
        let mut r = SchemaRegistry::new();
        let schema = Schema::new("Open", 1); // allowed_properties = None
        r.register(schema);
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("MATCH Open WHERE anything == \"x\" RETURN *").unwrap();
        assert!(a.analyze(&stmt).is_ok());
    }

    #[test]
    fn valid_delete() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("DELETE Person \"abc\"").unwrap();
        assert!(a.analyze(&stmt).is_ok());
    }

    #[test]
    fn delete_unknown_entity() {
        let r = registry_with_person();
        let a = SemanticAnalyzer::new(&r);
        let stmt = crate::parser::parse("DELETE Bogus \"abc\"").unwrap();
        let err = a.analyze(&stmt).unwrap_err();
        assert_eq!(err.code, diagnostics::Code::UnknownType);
    }
}
