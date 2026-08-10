//! Schema Registry — in-memory type schemas for validation.
//!
//! Increment-1 stores schemas in memory only (MRFC-0001). The kernel calls
//! `SchemaRegistry::validate` before committing a new version.

use crate::knowledge::kom::{KResult, KnowledgeObject, Schema};
use crate::KError;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct SchemaRegistry {
    schemas: HashMap<String, Schema>,
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, schema: Schema) {
        self.schemas.insert(schema.type_name.clone(), schema);
    }

    pub fn get(&self, type_name: &str) -> Option<&Schema> {
        self.schemas.get(type_name)
    }

    /// Validate an object against its registered schema, if one exists.
    /// Always runs the built-in `KnowledgeObject::validate` checks first.
    /// With MRFC-0060 Phase C1, also type-checks each property and enforces
    /// required + nullable constraints.
    /// When `skip_not_null` is true, the backend enforces NOT NULL so the kernel
    /// skips required-property and non-nullable checks. Type checking still runs.
    /// MRFC-0060 Phase C7.
    pub fn validate(&self, ko: &KnowledgeObject, skip_not_null: bool) -> KResult<()> {
        ko.validate()?;
        if let Some(schema) = self.schemas.get(&ko.metadata.type_name) {
            ko.validate_against(schema, skip_not_null)?;
            // MRFC-0060 Phase C1: property type checking
            for prop_def in &schema.properties {
                match ko.properties.get(&prop_def.name) {
                    Some(value) => {
                        value.type_check(prop_def).map_err(KError::InvalidSchema)?;
                    }
                    None => {
                        if !skip_not_null && prop_def.required {
                            return Err(KError::InvalidSchema(format!(
                                "missing required property: '{}'",
                                prop_def.name
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Check uniqueness constraints for this KO's type.
    /// When `skip_deferred` is true, constraints with `ConstraintTiming::Deferred`
    /// are skipped (caller must collect and evaluate them at commit time).
    /// When `write_set` is `Some(ws)`, constraints with no properties in the
    /// write-set are skipped (C6 incremental evaluation).
    /// MRFC-0060 Phase C2 / C5 / C6.
    pub fn check_uniqueness<F>(
        &self,
        ko: &KnowledgeObject,
        exists: F,
        skip_deferred: bool,
        write_set: Option<&HashSet<String>>,
    ) -> KResult<()>
    where
        F: Fn(
            crate::knowledge::kom::UniquenessScope,
            Option<&str>,
            &str,
            &[(String, crate::knowledge::kom::Value)],
            &crate::knowledge::kom::KOID,
        ) -> bool,
    {
        let schema = match self.schemas.get(&ko.metadata.type_name) {
            Some(s) => s,
            None => return Ok(()),
        };
        for (ci, constraint) in schema.unique_constraints.iter().enumerate() {
            if skip_deferred
                && constraint.timing == crate::knowledge::kom::ConstraintTiming::Deferred
            {
                continue;
            }
            // C6: skip constraints unaffected by write-set
            if !crate::lifecycle::constraint::unique_affected_by_write_set(constraint, write_set) {
                continue;
            }
            let mut pairs = Vec::with_capacity(constraint.properties.len());
            for prop_name in &constraint.properties {
                match ko.properties.get(prop_name.as_str()) {
                    Some(v) => pairs.push((prop_name.clone(), v.clone())),
                    None => {
                        return Err(KError::InvalidSchema(format!(
                            "unique constraint requires property '{}' but it is missing",
                            prop_name
                        )));
                    }
                }
            }
            if exists(
                constraint.scope,
                ko.metadata.tenant.as_deref(),
                &ko.metadata.type_name,
                &pairs,
                &ko.koid,
            ) {
                let names: Vec<&str> = constraint.properties.iter().map(|s| s.as_str()).collect();
                return Err(KError::InvalidSchema(format!(
                    "uniqueness constraint violated: ({}) already exists",
                    names.join(", ")
                )));
            }
            let _ = ci;
        }
        Ok(())
    }

    /// Collect deferred unique constraint entries for commit-time evaluation.
    /// Returns (constraint_index, property-value-pairs, scope) for each deferred constraint.
    /// MRFC-0060 Phase C5.
    pub fn collect_deferred_unique(
        &self,
        ko: &KnowledgeObject,
    ) -> Vec<(
        usize,
        Vec<(String, crate::knowledge::kom::Value)>,
        crate::knowledge::kom::UniquenessScope,
    )> {
        let schema = match self.schemas.get(&ko.metadata.type_name) {
            Some(s) => s,
            None => return vec![],
        };
        let mut entries = Vec::new();
        for (ci, constraint) in schema.unique_constraints.iter().enumerate() {
            if constraint.timing != crate::knowledge::kom::ConstraintTiming::Deferred {
                continue;
            }
            let mut pairs = Vec::with_capacity(constraint.properties.len());
            for prop_name in &constraint.properties {
                if let Some(v) = ko.properties.get(prop_name.as_str()) {
                    pairs.push((prop_name.clone(), v.clone()));
                }
            }
            if pairs.len() == constraint.properties.len() {
                entries.push((ci, pairs, constraint.scope));
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::{
        CheckExpression, CompareOp, DomainConstraint, KnowledgeObject, Metadata, PropertyMap,
        SecurityDescriptor, UniquenessScope, Value, KOID,
    };

    fn test_ko(type_name: &str) -> KnowledgeObject {
        KnowledgeObject::new(
            KOID::ZERO,
            Metadata {
                type_name: type_name.into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            SecurityDescriptor {
                owner: "tester".into(),
                acl: vec![],
                classification: None,
            },
        )
    }

    #[test]
    fn type_check_rejects_mismatched_property() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Person", 1).required_property("age", "Int"));
        let mut ko = test_ko("Person");
        ko.properties
            .insert("age".into(), Value::Text("forty-two".into()));
        let err = reg.validate(&ko, false).unwrap_err();
        assert!(matches!(err, crate::KError::InvalidSchema(_)));
    }

    #[test]
    fn type_check_passes_matching_property() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Person", 1).required_property("age", "Int"));
        let mut ko = test_ko("Person");
        ko.properties.insert("age".into(), Value::Int(42));
        assert!(reg.validate(&ko, false).is_ok());
    }

    #[test]
    fn required_property_missing_fails() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Person", 1).required_property("name", "Text"));
        let ko = test_ko("Person"); // no properties
        let err = reg.validate(&ko, false).unwrap_err();
        assert!(matches!(err, crate::KError::InvalidSchema(_)));
    }

    #[test]
    fn nullable_property_accepts_null() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Person", 1).nullable_property("nickname", "Text"));
        let mut ko = test_ko("Person");
        ko.properties.insert("nickname".into(), Value::Null);
        assert!(reg.validate(&ko, false).is_ok());
    }

    #[test]
    fn non_nullable_property_rejects_null() {
        let mut reg = SchemaRegistry::new();
        reg.register(
            Schema::new("Person", 1).property("name", "Text"), // nullable defaults to false
        );
        let mut ko = test_ko("Person");
        ko.properties.insert("name".into(), Value::Null);
        let err = reg.validate(&ko, false).unwrap_err();
        assert!(matches!(err, crate::KError::InvalidSchema(_)));
    }

    #[test]
    fn int_widens_to_float_in_schema_validation() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Stats", 1).required_property("score", "Float"));
        let mut ko = test_ko("Stats");
        ko.properties.insert("score".into(), Value::Int(95));
        assert!(reg.validate(&ko, false).is_ok());
    }

    #[test]
    fn unregistered_type_passes_validation() {
        let reg = SchemaRegistry::new();
        let ko = test_ko("NoSchema");
        assert!(reg.validate(&ko, false).is_ok());
    }

    #[test]
    fn multiple_typed_properties_all_validated() {
        let mut reg = SchemaRegistry::new();
        reg.register(
            Schema::new("Product", 1)
                .required_property("sku", "Text")
                .property("price", "Float")
                .nullable_property("description", "Text"),
        );
        let mut ko = test_ko("Product");
        ko.properties
            .insert("sku".into(), Value::Text("ABC-123".into()));
        ko.properties.insert("price".into(), Value::Float(9.99));
        ko.properties.insert("description".into(), Value::Null);
        assert!(reg.validate(&ko, false).is_ok());

        // price is wrong type
        ko.properties
            .insert("price".into(), Value::Text("cheap".into()));
        assert!(reg.validate(&ko, false).is_err());
    }

    // --- MRFC-0060 Phase C2: uniqueness tests ---

    #[test]
    fn unique_constraint_violation_rejected() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));
        let mut ko = test_ko("User");
        ko.properties
            .insert("email".into(), Value::Text("a@b.com".into()));

        // Simulate an existing KO with the same email
        let err = reg
            .check_uniqueness(
                &ko,
                |_, _, _, pairs, _| {
                    pairs
                        .iter()
                        .any(|(n, v)| n == "email" && v == &Value::Text("a@b.com".into()))
                },
                false,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, crate::KError::InvalidSchema(_)));
    }

    #[test]
    fn unique_constraint_passes_when_no_conflict() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));
        let mut ko = test_ko("User");
        ko.properties
            .insert("email".into(), Value::Text("new@b.com".into()));

        // No existing KO has this email
        assert!(reg
            .check_uniqueness(&ko, |_, _, _, _, _| false, false, None)
            .is_ok());
    }

    #[test]
    fn composite_unique_both_match_is_violation() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Org", 1).unique(&["tenant", "name"], UniquenessScope::Tenant));
        let mut ko = test_ko("Org");
        ko.properties
            .insert("tenant".into(), Value::Text("t1".into()));
        ko.properties
            .insert("name".into(), Value::Text("Acme".into()));

        // Simulate existing KO with same composite key
        let err = reg
            .check_uniqueness(
                &ko,
                |_, _, _, pairs, _| {
                    let has_tenant = pairs
                        .iter()
                        .any(|(n, v)| n == "tenant" && v == &Value::Text("t1".into()));
                    let has_name = pairs
                        .iter()
                        .any(|(n, v)| n == "name" && v == &Value::Text("Acme".into()));
                    has_tenant && has_name
                },
                false,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, crate::KError::InvalidSchema(_)));
    }

    #[test]
    fn composite_unique_partial_match_is_ok() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("Org", 1).unique(&["tenant", "name"], UniquenessScope::Tenant));
        let mut ko = test_ko("Org");
        ko.properties
            .insert("tenant".into(), Value::Text("t1".into()));
        ko.properties
            .insert("name".into(), Value::Text("NewCo".into()));

        // Only tenant matches, name is different → no violation
        assert!(reg
            .check_uniqueness(
                &ko,
                |_, _, _, pairs, _| {
                    // Simulate: tenant "t1" exists, but name "NewCo" doesn't match "Acme"
                    let has_tenant = pairs
                        .iter()
                        .any(|(n, v)| n == "tenant" && v == &Value::Text("t1".into()));
                    let has_name = pairs
                        .iter()
                        .any(|(n, v)| n == "name" && v == &Value::Text("Acme".into()));
                    has_tenant && has_name // partial match → false
                },
                false,
                None,
            )
            .is_ok());
    }

    #[test]
    fn unique_constraint_skips_excluded_koid() {
        let mut reg = SchemaRegistry::new();
        reg.register(Schema::new("User", 1).unique(&["email"], UniquenessScope::Type));
        let koid = KOID([1u8; 16]);
        let mut ko = test_ko("User");
        ko.koid = koid;
        ko.properties
            .insert("email".into(), Value::Text("same@b.com".into()));

        // Lookup returns same KOID → should be skipped (update case)
        assert!(reg
            .check_uniqueness(
                &ko,
                |_, _, _, _, exclude_koid| {
                    // Real lookup would skip this koid; mock returns false since
                    // the only match IS the excluded koid
                    exclude_koid != &koid
                },
                false,
                None,
            )
            .is_ok());
    }

    #[test]
    fn unique_constraint_no_schema_passes() {
        let reg = SchemaRegistry::new();
        let ko = test_ko("NoSchema");
        assert!(reg
            .check_uniqueness(&ko, |_, _, _, _, _| true, false, None)
            .is_ok());
    }

    // --- MRFC-0060 Phase C4: domain + check constraint validation ---
    // (Now evaluated via ConstraintEvaluator, not SchemaRegistry::validate)

    #[test]
    fn domain_range_rejected_in_validate() {
        let eval = crate::lifecycle::constraint::ConstraintEvaluator::new();
        let schema = Schema::new("Person", 1)
            .property("age", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(150.0),
            });
        let mut props = PropertyMap::new();
        props.insert("age".into(), Value::Int(-5));
        let err = eval.evaluate(&schema, &props, None).unwrap_err();
        assert!(format!("{}", err).contains("domain constraint"));
    }

    #[test]
    fn domain_length_rejected_in_validate() {
        let eval = crate::lifecycle::constraint::ConstraintEvaluator::new();
        let schema = Schema::new("Item", 1)
            .property("code", "Text")
            .domain_constraint(DomainConstraint::Length {
                min: Some(3),
                max: Some(20),
            });
        let mut props = PropertyMap::new();
        props.insert("code".into(), Value::Text("ab".into()));
        assert!(eval.evaluate(&schema, &props, None).is_err());
        props.insert("code".into(), Value::Text("valid-code".into()));
        assert!(eval.evaluate(&schema, &props, None).is_ok());
    }

    #[test]
    fn domain_enum_rejected_in_validate() {
        let eval = crate::lifecycle::constraint::ConstraintEvaluator::new();
        let schema = Schema::new("Task", 1)
            .property("status", "Text")
            .domain_constraint(DomainConstraint::Enum(vec![
                Value::Text("todo".into()),
                Value::Text("done".into()),
            ]));
        let mut props = PropertyMap::new();
        props.insert("status".into(), Value::Text("invalid".into()));
        assert!(eval.evaluate(&schema, &props, None).is_err());
    }

    #[test]
    fn check_constraint_evaluated_in_validate() {
        let eval = crate::lifecycle::constraint::ConstraintEvaluator::new();
        let schema = Schema::new("Event", 1)
            .property("end_date", "Text")
            .property("start_date", "Text")
            .check(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            );
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        let err = eval.evaluate(&schema, &props, None).unwrap_err();
        assert!(format!("{}", err).contains("end_ge_start"));
        // Fix it
        props.insert("end_date".into(), Value::Text("2024-12-31".into()));
        assert!(eval.evaluate(&schema, &props, None).is_ok());
    }
}
