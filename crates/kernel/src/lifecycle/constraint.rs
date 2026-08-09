//! Constraint Evaluator — domain + check constraint enforcement (MRFC-0060 Phase C4/C5).
//!
//! Separate from `SchemaRegistry` so constraints can be evaluated independently
//! of type/required/nullable/uniqueness checks. Called by the kernel during
//! `remember()` and `transact()`.

use crate::knowledge::kom::{
    ConstraintResult, ConstraintTiming, ConstraintViolation, InferenceCandidate, KResult,
    KnowledgeObject, PropertyMap, Schema, Value,
};
use crate::KError;
use std::collections::HashSet;

/// Stateless evaluator for domain and check constraints.
#[derive(Clone, Debug, Default)]
pub struct ConstraintEvaluator;

impl ConstraintEvaluator {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate domain constraints (always immediate) and immediate check constraints.
    /// Deferred check constraints are skipped — use `evaluate_deferred` for those.
    /// Fail-fast: returns on first violation.
    ///
    /// When `write_set` is `Some(ws)`: only evaluates constraints affected by the
    /// changed properties. `None` evaluates everything (create path).
    pub fn evaluate(
        &self,
        schema: &Schema,
        properties: &PropertyMap,
        write_set: Option<&HashSet<String>>,
    ) -> KResult<()> {
        // Skim: empty write-set on update means nothing changed — skip all.
        if let Some(ws) = write_set {
            if ws.is_empty() {
                return Ok(());
            }
        }
        // Domain constraints (always immediate)
        for prop_def in &schema.properties {
            if write_set.map_or(true, |ws| ws.contains(&prop_def.name)) {
                if let Some(value) = properties.get(&prop_def.name) {
                    for dc in &prop_def.domain_constraints {
                        dc.validate(value).map_err(|msg| {
                            KError::InvalidSchema(format!(
                                "property '{}' failed domain constraint: {}",
                                prop_def.name, msg
                            ))
                        })?;
                    }
                }
            }
        }
        // Immediate check constraints only
        for cc in &schema.check_constraints {
            if cc.timing != ConstraintTiming::Immediate {
                continue;
            }
            if !check_affected_by_write_set(cc, write_set) {
                continue;
            }
            match cc.predicate.evaluate(properties) {
                Ok(false) => {
                    return Err(KError::InvalidSchema(format!(
                        "check constraint '{}' failed",
                        cc.name
                    )));
                }
                Err(msg) => {
                    return Err(KError::InvalidSchema(format!(
                        "check constraint '{}' error: {}",
                        cc.name, msg
                    )));
                }
                Ok(true) => {}
            }
        }
        Ok(())
    }

    /// Evaluate all domain + check constraints for a single object, collecting every
    /// violation into a `ConstraintResult`. Does NOT check uniqueness — caller handles
    /// that separately. Used by `remember()` so the caller sees all errors at once.
    ///
    /// When `write_set` is `Some(ws)`: only evaluates constraints affected by the
    /// changed properties. `None` evaluates everything (create path).
    pub fn evaluate_full(
        &self,
        schema: &Schema,
        properties: &PropertyMap,
        write_set: Option<&HashSet<String>>,
        koid: Option<crate::knowledge::kom::KOID>,
        provenance_source: Option<&str>,
    ) -> ConstraintResult {
        let mut result = ConstraintResult::ok();
        // Skim: empty write-set on update means nothing changed.
        if let Some(ws) = write_set {
            if ws.is_empty() {
                return result;
            }
        }
        // Domain constraints — only for properties in write-set
        for prop_def in &schema.properties {
            if write_set.map_or(true, |ws| ws.contains(&prop_def.name)) {
                if let Some(value) = properties.get(&prop_def.name) {
                    for dc in &prop_def.domain_constraints {
                        if let Err(msg) = dc.validate(value) {
                            result.valid = false;
                            let mut v = ConstraintViolation::error(
                                &format!("domain.{}", prop_def.name),
                                &msg,
                            );
                            if let Some(k) = koid {
                                v = v.with_koid(k);
                            }
                            result.violations.push(v);
                        }
                    }
                }
            }
        }
        // Provenance-required properties (MRFC-0060 AC-17) — value must come from a
        // trusted source.  Flagged when the source is missing or empty.
        let has_source = provenance_source.map_or(false, |s| !s.is_empty());
        if !has_source {
            for prop_def in &schema.properties {
                if prop_def.provenance_required {
                    result.valid = false;
                    let mut v = ConstraintViolation::error(
                        &format!("provenance.{}", prop_def.name),
                        "property requires provenance but no source recorded",
                    );
                    if let Some(k) = koid {
                        v = v.with_koid(k);
                    }
                    result.violations.push(v);
                }
            }
        }

        // All check constraints (remember is single-object, deferred = immediate)
        for cc in &schema.check_constraints {
            if !check_affected_by_write_set(cc, write_set) {
                continue;
            }
            match cc.predicate.evaluate(properties) {
                Ok(false) => {
                    result.valid = false;
                    let mut v = ConstraintViolation::error(&cc.name, "check constraint failed");
                    if let Some(k) = koid {
                        v = v.with_koid(k);
                    }
                    result.violations.push(v);
                }
                Err(msg) => {
                    result.valid = false;
                    let mut v = ConstraintViolation::error(
                        &cc.name,
                        &format!("check constraint error: {}", msg),
                    );
                    if let Some(k) = koid {
                        v = v.with_koid(k);
                    }
                    result.violations.push(v);
                }
                Ok(true) => {}
            }
        }
        result
    }
}

/// True if the check constraint is affected by the write-set (or no write-set).
pub(crate) fn check_affected_by_write_set(
    cc: &crate::knowledge::kom::CheckConstraint,
    write_set: Option<&HashSet<String>>,
) -> bool {
    write_set.map_or(true, |ws| {
        cc.predicate
            .referenced_properties()
            .iter()
            .any(|p| ws.contains(*p))
    })
}

/// True if the unique constraint is affected by the write-set (or no write-set).
pub(crate) fn unique_affected_by_write_set(
    constraint: &crate::knowledge::kom::UniqueConstraint,
    write_set: Option<&HashSet<String>>,
) -> bool {
    write_set.map_or(true, |ws| {
        constraint
            .properties
            .iter()
            .any(|p| ws.contains(p.as_str()))
    })
}

// ---------------------------------------------------------------------------
// Transaction constraint state (MRFC-0060 Phase C5)
// ---------------------------------------------------------------------------

/// Accumulates deferred constraints during a transaction for commit-time evaluation.
#[derive(Clone, Debug, Default)]
pub struct TransactionConstraintState {
    /// Deferred uniqueness entries:
    /// (type_name, constraint_index, property-pairs, koid, scope, tenant).
    deferred_unique: Vec<(
        String,
        usize,
        Vec<(String, crate::knowledge::kom::Value)>,
        crate::knowledge::kom::KOID,
        crate::knowledge::kom::UniquenessScope,
        Option<String>,
    )>,
    /// Deferred check constraints: (type_name, constraint_index, koid, properties).
    deferred_checks: Vec<(String, usize, crate::knowledge::kom::KOID, PropertyMap)>,
}

impl TransactionConstraintState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a deferred uniqueness constraint for commit-time evaluation.
    pub fn record_unique(
        &mut self,
        type_name: &str,
        constraint_index: usize,
        pairs: Vec<(String, crate::knowledge::kom::Value)>,
        koid: crate::knowledge::kom::KOID,
        scope: crate::knowledge::kom::UniquenessScope,
        tenant: Option<String>,
    ) {
        self.deferred_unique.push((
            type_name.to_string(),
            constraint_index,
            pairs,
            koid,
            scope,
            tenant,
        ));
    }

    /// Record a deferred check constraint for commit-time evaluation.
    pub fn record_check(
        &mut self,
        type_name: &str,
        constraint_index: usize,
        koid: crate::knowledge::kom::KOID,
        properties: PropertyMap,
    ) {
        self.deferred_checks
            .push((type_name.to_string(), constraint_index, koid, properties));
    }

    /// True if there are no deferred constraints to evaluate.
    pub fn is_empty(&self) -> bool {
        self.deferred_unique.is_empty() && self.deferred_checks.is_empty()
    }
}

impl ConstraintEvaluator {
    /// Evaluate all deferred constraints collected during a transaction.
    ///
    /// `lookup` is called with (type_name, property-pairs, exclude_koid) and should
    /// return `true` if a conflicting object exists in storage. This mirrors the
    /// `check_uniqueness` callback pattern.
    ///
    /// `commit_ts` is stamped on every violation for diagnostics (0 = immediate).
    pub fn evaluate_deferred<F>(
        &self,
        state: &TransactionConstraintState,
        schemas: &crate::lifecycle::schema::SchemaRegistry,
        lookup: F,
        commit_ts: u64,
    ) -> ConstraintResult
    where
        F: Fn(
            crate::knowledge::kom::UniquenessScope,
            Option<&str>,
            &str,
            &[(String, crate::knowledge::kom::Value)],
            &crate::knowledge::kom::KOID,
        ) -> bool,
    {
        let mut result = ConstraintResult::ok();

        // --- Deferred uniqueness ---
        // Check within-batch conflicts first, then against storage.
        for (idx, (type_name, ci, pairs, koid, scope, tenant)) in
            state.deferred_unique.iter().enumerate()
        {
            // Within-batch: check all later entries, scope-aware.
            for (_jdx, (type_name2, ci2, pairs2, _koid2, _scope2, tenant2)) in
                state.deferred_unique.iter().enumerate().skip(idx + 1)
            {
                if ci == ci2 && pairs_match(pairs, pairs2) {
                    let in_scope = match scope {
                        crate::knowledge::kom::UniquenessScope::Type => type_name == type_name2,
                        crate::knowledge::kom::UniquenessScope::Tenant => match (tenant, tenant2) {
                            (Some(t1), Some(t2)) => t1 == t2,
                            _ => false,
                        },
                        crate::knowledge::kom::UniquenessScope::Global => true,
                    };
                    if in_scope {
                        let prop_names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
                        result.valid = false;
                        let mut v = ConstraintViolation::error(
                            &format!("{}.unique({})", type_name, prop_names.join(",")),
                            &format!(
                                "deferred uniqueness violated: ({}) conflicts within batch",
                                prop_names.join(", ")
                            ),
                        )
                        .with_koid(*koid);
                        v.timestamp = commit_ts;
                        result.violations.push(v);
                    }
                }
            }
            // Against storage
            if lookup(*scope, tenant.as_deref(), type_name, pairs, koid) {
                let prop_names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
                result.valid = false;
                let mut v = ConstraintViolation::error(
                    &format!("{}.unique({})", type_name, prop_names.join(",")),
                    &format!(
                        "deferred uniqueness violated: ({}) already exists",
                        prop_names.join(", ")
                    ),
                )
                .with_koid(*koid);
                v.timestamp = commit_ts;
                result.violations.push(v);
            }
        }

        // --- Deferred check constraints ---
        for (type_name, ci, koid, props) in &state.deferred_checks {
            if let Some(schema) = schemas.get(type_name) {
                if let Some(cc) = schema.check_constraints.get(*ci) {
                    match cc.predicate.evaluate(props) {
                        Ok(false) => {
                            result.valid = false;
                            let mut v = ConstraintViolation::error(
                                &cc.name,
                                &format!("deferred check constraint '{}' failed", cc.name),
                            )
                            .with_koid(*koid);
                            v.timestamp = commit_ts;
                            result.violations.push(v);
                        }
                        Err(msg) => {
                            result.valid = false;
                            let mut v = ConstraintViolation::error(
                                &cc.name,
                                &format!("deferred check constraint '{}' error: {}", cc.name, msg),
                            )
                            .with_koid(*koid);
                            v.timestamp = commit_ts;
                            result.violations.push(v);
                        }
                        Ok(true) => {}
                    }
                }
            }
        }

        result
    }
}

/// Check if two sets of (name, value) pairs match on all names and values.
fn pairs_match(
    a: &[(String, crate::knowledge::kom::Value)],
    b: &[(String, crate::knowledge::kom::Value)],
) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // Both are sorted by construction (property names are sorted in constraint definition).
    a.iter()
        .zip(b.iter())
        .all(|((an, av), (bn, bv))| an == bn && av == bv)
}

// ---------------------------------------------------------------------------
// Inference engine (MRFC-0060 Phase C8)
// ---------------------------------------------------------------------------

/// Stateless inference engine that scans existing data to discover constraint
/// candidates. Never auto-enforces — the caller reviews and manually registers.
#[derive(Clone, Debug, Default)]
pub struct InferenceEngine;

impl InferenceEngine {
    pub fn new() -> Self {
        Self
    }

    /// Scan KOs against a schema and produce constraint candidates.
    /// Caller provides the schema and all objects of that type.
    pub fn infer(&self, schema: &Schema, kos: &[KnowledgeObject]) -> Vec<InferenceCandidate> {
        if kos.is_empty() {
            return Vec::new();
        }
        let total_rows = kos.len();
        let mut candidates = Vec::new();

        for prop_def in &schema.properties {
            // --- NotNull ---
            if !prop_def.required {
                candidates.extend(self.infer_not_null(
                    &schema.type_name,
                    &prop_def.name,
                    kos,
                    total_rows,
                ));
            }
            // --- Uniqueness ---
            candidates.extend(self.infer_uniqueness(
                &schema.type_name,
                &prop_def.name,
                kos,
                total_rows,
            ));
            // --- Range (numeric only) ---
            candidates.extend(self.infer_range(&schema.type_name, &prop_def.name, kos, total_rows));
        }
        candidates
    }

    /// Count nulls and emit a NOT NULL candidate if confidence >= 0.9.
    fn infer_not_null(
        &self,
        type_name: &str,
        prop_name: &str,
        kos: &[KnowledgeObject],
        total_rows: usize,
    ) -> Option<InferenceCandidate> {
        let non_null = kos
            .iter()
            .filter(|ko| match ko.properties.get(prop_name) {
                Some(v) => !matches!(v, Value::Null),
                None => false,
            })
            .count();
        let confidence = non_null as f64 / total_rows as f64;
        if confidence >= 0.9 {
            // Null count is the inverse
            let null_count = total_rows - non_null;
            // Only emit if there ARE nulls to catch (otherwise confidence=1.0 with 0 nulls → pure info)
            // Emit anyway — 100% non-null is a strong signal
            Some(InferenceCandidate {
                type_name: type_name.into(),
                constraint_desc: format!("NOT NULL {}", prop_name),
                confidence,
                total_rows,
                violations: null_count,
            })
        } else {
            None
        }
    }

    /// Detect duplicate values; emit a UNIQUE candidate with confidence.
    fn infer_uniqueness(
        &self,
        type_name: &str,
        prop_name: &str,
        kos: &[KnowledgeObject],
        total_rows: usize,
    ) -> Option<InferenceCandidate> {
        // Collect non-null values
        let values: Vec<&Value> = kos
            .iter()
            .filter_map(|ko| ko.properties.get(prop_name))
            .filter(|v| !matches!(v, Value::Null))
            .collect();
        let non_null = values.len();
        if non_null < 2 {
            return None; // Can't infer uniqueness from 0-1 rows
        }
        // ponytail: O(n²) duplicate scan — inference is off the write path
        let mut duplicate_count = 0usize;
        for i in 0..values.len() {
            for j in (i + 1)..values.len() {
                if values[i] == values[j] {
                    duplicate_count += 1;
                    break; // Count each value once
                }
            }
        }
        let unique_count = non_null - duplicate_count;
        let confidence = unique_count as f64 / non_null as f64;
        Some(InferenceCandidate {
            type_name: type_name.into(),
            constraint_desc: format!("UNIQUE({})", prop_name),
            confidence,
            total_rows,
            violations: duplicate_count,
        })
    }

    /// Compute min/max for numeric properties; emit a range CHECK candidate.
    fn infer_range(
        &self,
        type_name: &str,
        prop_name: &str,
        kos: &[KnowledgeObject],
        total_rows: usize,
    ) -> Option<InferenceCandidate> {
        let mut min: Option<f64> = None;
        let mut max: Option<f64> = None;
        for ko in kos {
            match ko.properties.get(prop_name) {
                Some(Value::Int(n)) => {
                    let n = *n as f64;
                    min = Some(min.map_or(n, |m| m.min(n)));
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
                Some(Value::Float(n)) => {
                    let n = *n;
                    min = Some(min.map_or(n, |m| m.min(n)));
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
                _ => {} // skip non-numeric and null
            }
        }
        match (min, max) {
            (Some(lo), Some(hi)) => {
                if lo == hi {
                    return None; // Constant value — not useful as a range constraint
                }
                Some(InferenceCandidate {
                    type_name: type_name.into(),
                    constraint_desc: format!(
                        "CHECK {} >= {} AND {} <= {}",
                        prop_name, lo, prop_name, hi
                    ),
                    confidence: 1.0, // Range describes data, not a rule it violates
                    total_rows,
                    violations: 0,
                })
            }
            _ => None, // No numeric values found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::kom::{
        CheckExpression, CompareOp, DomainConstraint, UniquenessScope, Value, ViolationSeverity,
        KOID,
    };
    use crate::lifecycle::schema::SchemaRegistry;

    fn test_props() -> PropertyMap {
        let mut props = PropertyMap::new();
        props.insert("name".into(), Value::Text("Alice".into()));
        props.insert("age".into(), Value::Int(30));
        props
    }

    #[test]
    fn evaluator_passes_valid_data() {
        let schema = crate::knowledge::kom::Schema::new("Person", 1)
            .property("age", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(150.0),
            });
        let eval = ConstraintEvaluator::new();
        let props = test_props();
        assert!(eval.evaluate(&schema, &props, None).is_ok());
    }

    #[test]
    fn evaluator_rejects_domain_violation() {
        let schema = crate::knowledge::kom::Schema::new("Person", 1)
            .property("age", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(150.0),
            });
        let eval = ConstraintEvaluator::new();
        let mut props = test_props();
        props.insert("age".into(), Value::Int(-5));
        let err = eval.evaluate(&schema, &props, None).unwrap_err();
        assert!(format!("{}", err).contains("domain constraint"));
    }

    #[test]
    fn evaluator_rejects_check_violation() {
        let schema = crate::knowledge::kom::Schema::new("Event", 1)
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
        let eval = ConstraintEvaluator::new();
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        let err = eval.evaluate(&schema, &props, None).unwrap_err();
        assert!(format!("{}", err).contains("end_ge_start"));
    }

    #[test]
    fn evaluator_no_schema_properties_is_noop() {
        let schema = crate::knowledge::kom::Schema::new("Empty", 1);
        let eval = ConstraintEvaluator::new();
        let props = test_props();
        assert!(eval.evaluate(&schema, &props, None).is_ok());
    }

    #[test]
    fn evaluator_skips_deferred_check_constraints() {
        let schema = crate::knowledge::kom::Schema::new("Event", 1)
            .property("start_date", "Text")
            .property("end_date", "Text")
            .check_deferred(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            );
        let eval = ConstraintEvaluator::new();
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        // end_date < start_date would fail, but it's deferred so evaluate() ignores it
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        assert!(eval.evaluate(&schema, &props, None).is_ok());
    }

    // --- evaluate_full tests ---

    #[test]
    fn evaluate_full_collects_all_violations() {
        let schema = crate::knowledge::kom::Schema::new("Person", 1)
            .property("age", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(150.0),
            })
            .property("name", "Text")
            .domain_constraint(DomainConstraint::Length {
                min: Some(3),
                max: Some(50),
            })
            .check(
                "age_positive",
                CheckExpression::Compare {
                    op: CompareOp::Gt,
                    left: Box::new(CheckExpression::Property("age".into())),
                    right: Box::new(CheckExpression::Literal(Value::Int(0))),
                },
            );
        let eval = ConstraintEvaluator::new();
        let mut props = PropertyMap::new();
        props.insert("age".into(), Value::Int(-5)); // violates Range
        props.insert("name".into(), Value::Text("ab".into())); // violates Length
        let result = eval.evaluate_full(&schema, &props, None, None, None);
        assert!(!result.valid);
        // age=-5 violates Range, name="ab" violates Length, -5 > 0 violates check
        assert_eq!(result.violations.len(), 3);
    }

    #[test]
    fn evaluate_full_includes_deferred_checks() {
        let schema = crate::knowledge::kom::Schema::new("Event", 1)
            .property("end_date", "Text")
            .property("start_date", "Text")
            .check_deferred(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            );
        let eval = ConstraintEvaluator::new();
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        // evaluate_full checks ALL constraints (remember is single-object)
        let result = eval.evaluate_full(&schema, &props, None, None, None);
        assert!(!result.valid);
        assert!(result.violations[0]
            .constraint_name
            .contains("end_ge_start"));
    }

    #[test]
    fn evaluate_full_passes_valid_data() {
        let schema = crate::knowledge::kom::Schema::new("Person", 1)
            .property("age", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(150.0),
            });
        let eval = ConstraintEvaluator::new();
        let props = test_props();
        let result = eval.evaluate_full(&schema, &props, None, None, None);
        assert!(result.valid);
        assert!(result.violations.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn constraint_result_into_kresult_ok() {
        let result = ConstraintResult::ok();
        assert!(result.into_kresult().is_ok());
    }

    #[test]
    fn constraint_result_into_kresult_with_violations() {
        let mut result = ConstraintResult::ok();
        result.valid = false;
        result
            .violations
            .push(ConstraintViolation::error("c1", "bad thing"));
        let err = result.into_kresult().unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("c1") && msg.contains("bad thing"));
    }

    #[test]
    fn constraint_result_merge_combines_violations() {
        let mut r1 = ConstraintResult::ok();
        r1.valid = false;
        r1.violations.push(ConstraintViolation::error("c1", "err1"));
        let mut r2 = ConstraintResult::ok();
        r2.warnings
            .push(ConstraintViolation::warning("c2", "warn2"));
        r1.merge(&r2);
        assert!(!r1.valid);
        assert_eq!(r1.violations.len(), 1);
        assert_eq!(r1.warnings.len(), 1);
        assert_eq!(r1.warnings[0].severity, ViolationSeverity::Warning);
    }

    // --- TransactionConstraintState tests ---

    #[test]
    fn deferred_state_starts_empty() {
        let state = TransactionConstraintState::new();
        assert!(state.is_empty());
    }

    #[test]
    fn deferred_unique_within_batch_conflict() {
        let mut reg = SchemaRegistry::new();
        reg.register(
            crate::knowledge::kom::Schema::new("User", 1)
                .unique_deferred(&["email"], UniquenessScope::Type),
        );
        let mut state = TransactionConstraintState::new();
        let k1 = KOID::from_bytes([1u8; 16]);
        let k2 = KOID::from_bytes([2u8; 16]);
        state.record_unique(
            "User",
            0,
            vec![("email".into(), Value::Text("a@b.com".into()))],
            k1,
            UniquenessScope::Type,
            None,
        );
        state.record_unique(
            "User",
            0,
            vec![("email".into(), Value::Text("a@b.com".into()))],
            k2,
            UniquenessScope::Type,
            None,
        );

        let eval = ConstraintEvaluator::new();
        let result = eval.evaluate_deferred(&state, &reg, |_, _, _, _, _| false, 42);
        assert!(!result.valid);
        assert_eq!(result.violations.len(), 1);
        assert!(result.violations[0].message.contains("within batch"));
        assert_eq!(result.violations[0].timestamp, 42);
        assert_eq!(result.violations[0].severity, ViolationSeverity::Error);
    }

    #[test]
    fn deferred_unique_storage_conflict() {
        let mut reg = SchemaRegistry::new();
        reg.register(
            crate::knowledge::kom::Schema::new("User", 1)
                .unique_deferred(&["email"], UniquenessScope::Type),
        );
        let mut state = TransactionConstraintState::new();
        let k1 = KOID::from_bytes([1u8; 16]);
        state.record_unique(
            "User",
            0,
            vec![("email".into(), Value::Text("exists@b.com".into()))],
            k1,
            UniquenessScope::Type,
            None,
        );

        let eval = ConstraintEvaluator::new();
        let result = eval.evaluate_deferred(
            &state,
            &reg,
            |_, _, _, pairs, _| {
                pairs
                    .iter()
                    .any(|(n, v)| n == "email" && v == &Value::Text("exists@b.com".into()))
            },
            99,
        );
        assert!(!result.valid);
        assert!(result.violations[0].message.contains("already exists"));
        assert_eq!(result.violations[0].timestamp, 99);
    }

    #[test]
    fn deferred_check_evaluated_at_commit_time() {
        let mut reg = SchemaRegistry::new();
        reg.register(
            crate::knowledge::kom::Schema::new("Event", 1)
                .property("end_date", "Text")
                .property("start_date", "Text")
                .check_deferred(
                    "end_ge_start",
                    CheckExpression::Compare {
                        op: CompareOp::Gte,
                        left: Box::new(CheckExpression::Property("end_date".into())),
                        right: Box::new(CheckExpression::Property("start_date".into())),
                    },
                ),
        );
        let mut state = TransactionConstraintState::new();
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        state.record_check("Event", 0, KOID::from_bytes([1u8; 16]), props);

        let eval = ConstraintEvaluator::new();
        let result = eval.evaluate_deferred(&state, &reg, |_, _, _, _, _| false, 7);
        assert!(!result.valid);
        assert!(result.violations[0].message.contains("end_ge_start"));
        assert_eq!(result.violations[0].timestamp, 7);
    }

    // --- C6: write-set filtering ---

    #[test]
    fn evaluate_full_skips_unaffected_check() {
        // Check on end_date >= start_date — only affected by those two properties.
        let schema = crate::knowledge::kom::Schema::new("Event", 1)
            .property("title", "Text")
            .property("start_date", "Text")
            .property("end_date", "Text")
            .check(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            );
        let eval = ConstraintEvaluator::new();
        // Only "title" changed — constraint references {"end_date", "start_date"}.
        let mut props = PropertyMap::new();
        props.insert("title".into(), Value::Text("NewTitle".into()));
        let mut ws = HashSet::new();
        ws.insert("title".to_string());
        let result = eval.evaluate_full(&schema, &props, Some(&ws), None, None);
        // Should pass: constraint is unaffected by "title" change.
        assert!(result.valid);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn evaluate_full_runs_affected_check() {
        let schema = crate::knowledge::kom::Schema::new("Event", 1)
            .property("start_date", "Text")
            .property("end_date", "Text")
            .check(
                "end_ge_start",
                CheckExpression::Compare {
                    op: CompareOp::Gte,
                    left: Box::new(CheckExpression::Property("end_date".into())),
                    right: Box::new(CheckExpression::Property("start_date".into())),
                },
            );
        let eval = ConstraintEvaluator::new();
        // "end_date" changed → write-set includes it → constraint runs and fails.
        let mut props = PropertyMap::new();
        props.insert("start_date".into(), Value::Text("2024-06-01".into()));
        props.insert("end_date".into(), Value::Text("2024-01-01".into()));
        let mut ws = HashSet::new();
        ws.insert("end_date".to_string());
        let result = eval.evaluate_full(&schema, &props, Some(&ws), None, None);
        assert!(!result.valid);
        assert!(result.violations[0]
            .constraint_name
            .contains("end_ge_start"));
    }

    #[test]
    fn evaluate_full_skim_empty_write_set() {
        // Empty write-set → skip all constraints (identity update).
        let schema = crate::knowledge::kom::Schema::new("Item", 1)
            .property("name", "Text")
            .check(
                "name_not_empty",
                CheckExpression::Compare {
                    op: CompareOp::Neq,
                    left: Box::new(CheckExpression::Property("name".into())),
                    right: Box::new(CheckExpression::Literal(Value::Text("".into()))),
                },
            );
        let eval = ConstraintEvaluator::new();
        let props = PropertyMap::new();
        // Props missing "name" would normally fail the check, but...
        let empty_ws: HashSet<String> = HashSet::new();
        let result = eval.evaluate_full(&schema, &props, Some(&empty_ws), None, None);
        // ...empty write-set skims ALL evaluation.
        assert!(result.valid);
    }

    #[test]
    fn violation_includes_koid_when_provided() {
        let eval = ConstraintEvaluator::new();
        let schema = Schema::new("Item", 1)
            .property("price", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(100.0),
            });
        let mut props = PropertyMap::new();
        props.insert("price".into(), Value::Int(-5));
        let koid = KOID([7u8; 16]);
        let result = eval.evaluate_full(&schema, &props, None, Some(koid), None);
        assert!(!result.valid);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].koid, Some(koid));
    }

    #[test]
    fn violation_koid_none_when_not_provided() {
        let eval = ConstraintEvaluator::new();
        let schema = Schema::new("Item", 1)
            .property("price", "Int")
            .domain_constraint(DomainConstraint::Range {
                min: Some(0.0),
                max: Some(100.0),
            });
        let mut props = PropertyMap::new();
        props.insert("price".into(), Value::Int(-5));
        let result = eval.evaluate_full(&schema, &props, None, None, None);
        assert!(!result.valid);
        assert_eq!(result.violations[0].koid, None);
    }

    // --- C8: inference engine ---

    fn make_ko(type_name: &str, idx: u8, props: Vec<(&str, Value)>) -> KnowledgeObject {
        let metadata = crate::knowledge::kom::Metadata {
            type_name: type_name.into(),
            tenant: None,
            schema_version: 1,
            tags: vec![],
        };
        let security = crate::knowledge::kom::SecurityDescriptor {
            owner: "test".into(),
            acl: vec![],
            classification: None,
        };
        let mut ko = KnowledgeObject::new(KOID::from_bytes([idx; 16]), metadata, security);
        for (k, v) in props {
            ko.properties.insert(k.into(), v);
        }
        ko
    }

    #[test]
    fn inference_empty_kos_returns_nothing() {
        let schema = crate::knowledge::kom::Schema::new("Test", 1).property("name", "Text");
        let engine = InferenceEngine::new();
        let candidates = engine.infer(&schema, &[]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn inference_uniqueness_perfect() {
        let schema = crate::knowledge::kom::Schema::new("User", 1).property("email", "Text");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("User", 1, vec![("email", Value::Text("a@b.com".into()))]),
            make_ko("User", 2, vec![("email", Value::Text("c@d.com".into()))]),
            make_ko("User", 3, vec![("email", Value::Text("e@f.com".into()))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let unique = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("UNIQUE"))
            .unwrap();
        assert_eq!(unique.confidence, 1.0);
        assert_eq!(unique.violations, 0);
        assert_eq!(unique.total_rows, 3);
    }

    #[test]
    fn inference_uniqueness_with_duplicates() {
        let schema = crate::knowledge::kom::Schema::new("User", 1).property("email", "Text");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("User", 1, vec![("email", Value::Text("a@b.com".into()))]),
            make_ko("User", 2, vec![("email", Value::Text("a@b.com".into()))]), // dup
            make_ko("User", 3, vec![("email", Value::Text("c@d.com".into()))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let unique = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("UNIQUE"))
            .unwrap();
        assert!(unique.confidence < 1.0);
        assert_eq!(unique.violations, 1); // one duplicate value
    }

    #[test]
    fn inference_uniqueness_skips_nulls() {
        let schema = crate::knowledge::kom::Schema::new("User", 1).property("email", "Text");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("User", 1, vec![("email", Value::Null)]),
            make_ko("User", 2, vec![("email", Value::Null)]),
            make_ko("User", 3, vec![("email", Value::Text("only@b.com".into()))]),
            make_ko("User", 4, vec![("email", Value::Text("also@b.com".into()))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let unique = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("UNIQUE"))
            .unwrap();
        // Two non-null distinct values → 100% unique among non-nulls
        assert_eq!(unique.confidence, 1.0);
        assert_eq!(unique.violations, 0);
    }

    #[test]
    fn inference_not_null_emits_when_confident() {
        let schema = crate::knowledge::kom::Schema::new("User", 1).property("age", "Int");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("User", 1, vec![("age", Value::Int(30))]),
            make_ko("User", 2, vec![("age", Value::Int(25))]),
            make_ko("User", 3, vec![("age", Value::Int(40))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let not_null = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("NOT NULL"));
        assert!(not_null.is_some());
        let nn = not_null.unwrap();
        assert_eq!(nn.confidence, 1.0);
        assert_eq!(nn.violations, 0);
    }

    #[test]
    fn inference_not_null_skips_low_confidence() {
        let schema = crate::knowledge::kom::Schema::new("User", 1).property("nickname", "Text");
        let engine = InferenceEngine::new();
        let mut kos = Vec::new();
        for i in 0..10 {
            if i < 5 {
                kos.push(make_ko(
                    "User",
                    i,
                    vec![("nickname", Value::Text("x".into()))],
                ));
            } else {
                kos.push(make_ko("User", i, vec![])); // absent = null
            }
        }
        let candidates = engine.infer(&schema, &kos);
        let not_null = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("NOT NULL"));
        // 5/10 = 0.5 confidence, below 0.9 threshold
        assert!(not_null.is_none());
    }

    #[test]
    fn inference_not_null_skips_already_required() {
        let schema =
            crate::knowledge::kom::Schema::new("User", 1).required_property("name", "Text"); // required=true on property
        let engine = InferenceEngine::new();
        let kos = vec![make_ko(
            "User",
            1,
            vec![("name", Value::Text("Alice".into()))],
        )];
        let candidates = engine.infer(&schema, &kos);
        let not_null = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("NOT NULL") && c.constraint_desc.contains("name"));
        assert!(not_null.is_none());
    }

    #[test]
    fn inference_range_numeric() {
        let schema = crate::knowledge::kom::Schema::new("Item", 1).property("score", "Int");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("Item", 1, vec![("score", Value::Int(10))]),
            make_ko("Item", 2, vec![("score", Value::Int(50))]),
            make_ko("Item", 3, vec![("score", Value::Int(99))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let range = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("CHECK") && c.constraint_desc.contains("score"));
        assert!(range.is_some());
        let r = range.unwrap();
        assert!(r.constraint_desc.contains(">= 10"));
        assert!(r.constraint_desc.contains("<= 99"));
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn inference_range_constant_skipped() {
        let schema = crate::knowledge::kom::Schema::new("Item", 1).property("score", "Int");
        let engine = InferenceEngine::new();
        let kos = vec![
            make_ko("Item", 1, vec![("score", Value::Int(42))]),
            make_ko("Item", 2, vec![("score", Value::Int(42))]),
        ];
        let candidates = engine.infer(&schema, &kos);
        let range = candidates
            .iter()
            .find(|c| c.constraint_desc.contains("CHECK") && c.constraint_desc.contains("score"));
        assert!(range.is_none()); // constant value, not useful as range
    }
}
