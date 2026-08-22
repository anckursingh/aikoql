//! Aikoql Reasoning Engine — rule execution over Knowledge Objects.
//!
//! Consumes Knowledge Events from the scheduler, evaluates registered rules
//! against committed KOs, and writes back provenance-tagged claims with
//! `origin=Reason`. Rules are themselves KOs of type `aikoql:rule`.
//!
//! MRFC-0005 §Knowledge Services: Rule execution, ontology processing,
//! provenance and inference. All work is async, off the commit path, and
//! writes back versioned claims — never silent mutation.

use aikoql_kernel::knowledge::kom::*;
use aikoql_kernel::transaction::kernel::{DeriveRequest, Kernel, Subject};
use aikoql_scheduler::SchedulerJob;
use std::sync::RwLock;

// ---------------------------------------------------------------------------
// Rule format
// ---------------------------------------------------------------------------

/// A single condition in a rule: "property P equals value V".
type Condition = (String, Value);

/// A loaded rule ready for evaluation.
#[derive(Clone, Debug)]
struct Rule {
    koid: KOID,
    conditions: Vec<Condition>,
    conclusion_type: String,
    conclusion_props: PropertyMap,
}

// ---------------------------------------------------------------------------
// ReasoningEngine
// ---------------------------------------------------------------------------

/// Rule engine that evaluates if-then rules against committed KOs.
/// Implements `SchedulerJob` so it plugs into the Scheduler's event loop.
pub struct ReasoningEngine {
    rules: RwLock<Vec<Rule>>,
    water: RwLock<u64>,
}

impl ReasoningEngine {
    pub fn new() -> Self {
        ReasoningEngine {
            rules: RwLock::new(Vec::new()),
            water: RwLock::new(0),
        }
    }

    /// Load a rule from a KO of type `aikoql:rule`.
    /// Expected properties: `if` (list of {property, value} maps) and
    /// `then_type` (string) + `then` (property map for the conclusion).
    fn load_rule(&self, ko: &KnowledgeObject) -> KResult<()> {
        let conditions: Vec<Condition> = match ko.properties.get("if") {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Map(m) => {
                        let prop = match m.get("property") {
                            Some(Value::Text(p)) => p.clone(),
                            _ => return None,
                        };
                        let val = m.get("value").cloned().unwrap_or(Value::Null);
                        Some((prop, val))
                    }
                    _ => None,
                })
                .collect(),
            _ => return Ok(()), // no conditions = rule never fires
        };

        let conclusion_type = match ko.properties.get("then_type") {
            Some(Value::Text(t)) => t.clone(),
            _ => return Ok(()),
        };

        let conclusion_props = match ko.properties.get("then") {
            Some(Value::Map(m)) => m.clone(),
            _ => PropertyMap::new(),
        };

        // justified: RwLock poison is unrecoverable
        self.rules.write().unwrap().push(Rule {
            koid: ko.koid,
            conditions,
            conclusion_type,
            conclusion_props,
        });
        Ok(())
    }

    /// Evaluate all registered rules against a KO. For each rule where all
    /// conditions match, derive the conclusion as a new KO — a first-class
    /// derivation (K3): premises (matched KO + rule) wired as DERIVED_FROM
    /// edges, operation/actor/reason recorded, origin=Reason.
    fn evaluate(&self, kernel: &Kernel, ko: &KnowledgeObject) -> KResult<()> {
        // justified: RwLock poison is unrecoverable
        let rules = self.rules.read().unwrap();
        for rule in rules.iter() {
            let all_match = rule
                .conditions
                .iter()
                .all(|(prop, expected)| ko.properties.get(prop) == Some(expected));
            if !all_match {
                continue;
            }
            // Derive the conclusion (anti-CRUD-cosplay: not a bare write).
            let mut req = DeriveRequest::new(
                aikoql_kernel::Subject::new("reasoning-engine"),
                rule.conclusion_type.clone(),
            );
            req.properties = rule.conclusion_props.clone();
            req.sources = vec![ko.koid, rule.koid];
            req.operation = "rule_fired".into();
            req.actor = "reasoning-engine".into();
            req.reason = Some(format!("rule {} fired on {}", rule.koid, ko.koid));
            // Fire-and-forget: don't fail the evaluation if one derivation fails.
            let _ = kernel.derive(req);
        }
        Ok(())
    }
}

impl Default for ReasoningEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SchedulerJob for ReasoningEngine {
    fn name(&self) -> &str {
        "reasoning-engine"
    }

    fn start(&self, kernel: &Kernel) -> KResult<()> {
        let subject = Subject::new("reasoning-engine");
        // Load all existing rules.
        for rule_ko in kernel.scan_by_type(&subject, "aikoql:rule")? {
            self.load_rule(&rule_ko)?;
        }
        // Evaluate all non-rule KOs against loaded rules.
        let all = kernel.scan_by_type(&subject, "fact")?;
        // ponytail: scan_by_type per type; add scan_all_types if enumeration
        // across all types becomes a real workload.
        for ko in &all {
            self.evaluate(kernel, ko)?;
        }
        Ok(())
    }

    fn shutdown(&self) {
        // Stateless — no background thread to stop.
    }

    fn checkpoint(&self, _dir: &std::path::Path) -> KResult<()> {
        // Rules are persisted as KOs; no additional state to checkpoint.
        Ok(())
    }

    fn water(&self) -> u64 {
        // justified: RwLock poison is unrecoverable
        *self.water.read().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aikoql_kernel::{ManualClock, MemoryEngine, Metadata, RememberRequest};
    use aikoql_scheduler::Scheduler;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn mk() -> Kernel {
        let clock = Arc::new(ManualClock::new(20_000));
        Kernel::open(Arc::new(MemoryEngine::new()), clock, 0xDECAF).unwrap()
    }

    #[test]
    fn rule_fires_and_produces_claim() {
        let k = mk();
        let engine_subject = Subject::new("reasoning-engine");

        // Register a rule: if a fact has "temperature" > 30, assert a "warning".
        let mut rule_props = PropertyMap::new();
        rule_props.insert(
            "if".into(),
            Value::List(vec![Value::Map(BTreeMap::from([
                ("property".into(), Value::Text("temperature".into())),
                ("value".into(), Value::Int(35)),
            ]))]),
        );
        rule_props.insert("then_type".into(), Value::Text("warning".into()));
        rule_props.insert(
            "then".into(),
            Value::Map(BTreeMap::from([(
                "message".into(),
                Value::Text("high temperature".into()),
            )])),
        );

        let rule_req = RememberRequest {
            context: (&engine_subject).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "aikoql:rule".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: rule_props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        };
        let rule_koid = k.remember(rule_req).unwrap().koid;

        // Create a fact that matches the rule.
        let mut fact_props = PropertyMap::new();
        fact_props.insert("temperature".into(), Value::Int(35));
        let fact_req = RememberRequest {
            context: (&engine_subject).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: fact_props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        };
        let fact_koid = k.remember(fact_req).unwrap().koid;

        // Start the reasoning engine via the scheduler.
        let engine = Arc::new(ReasoningEngine::new());
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        // The engine should have fired the rule, producing a warning.
        let warnings = k.scan_by_type(&engine_subject, "warning").unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].properties.get("message"),
            Some(&Value::Text("high temperature".into()))
        );
        // Provenance must be tagged.
        assert_eq!(warnings[0].lifecycle.origin, Origin::Reason);
        // K3: the conclusion is a first-class derivation — premises wired as
        // DERIVED_FROM edges, operation/actor/reason stamped, Inferred status.
        let d = warnings[0]
            .derivation()
            .expect("derivation record must be stamped");
        assert_eq!(d.operation, "rule_fired");
        assert_eq!(d.actor, "reasoning-engine");
        assert_eq!(d.sources, vec![fact_koid, rule_koid]);
        assert_eq!(warnings[0].epistemic_status(), EpistemicStatus::Inferred);
        assert_eq!(
            k.outbound_edges(&fact_koid, Some(DERIVED_FROM)).unwrap(),
            vec![("derived_from".to_string(), warnings[0].koid)]
        );
    }

    #[test]
    fn rule_does_not_fire_on_non_matching_fact() {
        let k = mk();
        let engine_subject = Subject::new("reasoning-engine");

        // Rule: temperature == 35 → warning.
        let mut rule_props = PropertyMap::new();
        rule_props.insert(
            "if".into(),
            Value::List(vec![Value::Map(BTreeMap::from([
                ("property".into(), Value::Text("temperature".into())),
                ("value".into(), Value::Int(35)),
            ]))]),
        );
        rule_props.insert("then_type".into(), Value::Text("warning".into()));
        rule_props.insert("then".into(), Value::Map(BTreeMap::new()));

        k.remember(RememberRequest {
            context: (&engine_subject).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "aikoql:rule".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: rule_props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();

        // Fact with non-matching temperature.
        let mut fact_props = PropertyMap::new();
        fact_props.insert("temperature".into(), Value::Int(20));
        k.remember(RememberRequest {
            context: (&engine_subject).into(),
            koid: None,
            expected_version: Some(0),
            idempotency_key: None,
            metadata: Metadata {
                type_name: "fact".into(),
                tenant: None,
                schema_version: 1,
                tags: vec![],
            },
            properties: fact_props,
            semantic: None,
            relationships: vec![],
            security: None,
            extensions: ExtensionMap::new(),
            origin: Origin::Human,
            note: None,
            referential_policy: ReferentialPolicy::default(),
        })
        .unwrap();

        let engine = Arc::new(ReasoningEngine::new());
        let sched = Scheduler::new();
        sched.register(engine);
        sched.start_all(&k).unwrap();

        let warnings = k.scan_by_type(&engine_subject, "warning").unwrap();
        assert!(warnings.is_empty());
    }
}
