//! A2 code parsers: golden fixtures per language (MRFC-0070 Phase A2).
//!
//! Exact-match asserts = 100% precision/recall on the labeled fixture — the
//! entities/relations the AST walks must produce, nothing more. AKI-004's
//! 90/85 targets on larger real-world corpora and AKI-019/020 traceability
//! fixtures remain future benchmarks (noted in TESTING-PLAN).

use aikoql_ingestion::{compile_java_source, compile_python_source, compile_ts_source};
use std::collections::BTreeSet;

fn entity_set(ir: &aikoql_ingestion::KnowledgeIr) -> BTreeSet<(String, String)> {
    ir.entities
        .iter()
        .map(|e| (e.name.clone(), e.type_hint.clone().unwrap_or_default()))
        .collect()
}

fn rel_set(ir: &aikoql_ingestion::KnowledgeIr, predicate: &str) -> BTreeSet<(String, String)> {
    ir.relations
        .iter()
        .filter(|r| r.predicate == predicate)
        .map(|r| (r.subject.clone(), r.object.clone()))
        .collect()
}

fn fact_statements(ir: &aikoql_ingestion::KnowledgeIr) -> BTreeSet<String> {
    ir.facts.iter().map(|f| f.statement.clone()).collect()
}

#[test]
fn a2_python_golden_fixture() {
    let src = r#""""Payment service module."""

import os
from aikoql.kernel import kom

class PaymentService:
    """Handles payment processing.

    Uses MVCC for isolation.
    """
    def charge(self, amount: int) -> bool:
        """Charge the given amount."""
        return True

class RetryPolicy:
    """Policy for retrying failed charges."""
    def should_retry(self) -> bool:
        return True

def test_charge_roundtrip():
    """Round-trip test."""
    pass
"#;

    let ir = compile_python_source(src, Some("payments.py"));

    let want_entities: BTreeSet<(String, String)> = [
        ("PaymentService", "Class"),
        ("PaymentService::charge", "Method"),
        ("RetryPolicy", "Class"),
        ("RetryPolicy::should_retry", "Method"),
        ("test_charge_roundtrip", "Test"),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_string(), t.to_string()))
    .collect();
    assert_eq!(entity_set(&ir), want_entities, "exact python entity set");

    let want_depends: BTreeSet<(String, String)> = [("module", "os"), ("module", "aikoql.kernel")]
        .into_iter()
        .map(|(s, o)| (s.to_string(), o.to_string()))
        .collect();
    assert_eq!(
        rel_set(&ir, "depends_on"),
        want_depends,
        "exact python imports"
    );

    let want_tests: BTreeSet<(String, String)> = [("test_charge_roundtrip", "module")]
        .into_iter()
        .map(|(s, o)| (s.to_string(), o.to_string()))
        .collect();
    assert_eq!(rel_set(&ir, "tested_by"), want_tests, "exact python tests");

    let want_facts: BTreeSet<String> = [
        "Payment service module.",
        "Handles payment processing.",
        "Uses MVCC for isolation.",
        "Charge the given amount.",
        "Policy for retrying failed charges.",
        "Round-trip test.",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(fact_statements(&ir), want_facts, "exact python doc facts");
}

#[test]
fn a2_typescript_golden_fixture() {
    let src = r#"/**
 * Payment service implementation.
 */
import { Kernel } from './kernel';
import os from 'node:os';

export class PaymentService implements Gateway {
  /** Charges the given amount. */
  charge(amount: number): boolean {
    return true;
  }
}

export interface Gateway {
  process(): void;
}

export class LegacyPayment extends PaymentService {
  /** Legacy charge path. */
  charge(amount: number): boolean {
    return false;
  }
}

describe('PaymentService', () => {
  it('charges successfully', () => {
    expect(true).toBe(true);
  });
});
"#;

    let ir = compile_ts_source(src, Some("payments.ts"));

    let want_entities: BTreeSet<(String, String)> = [
        ("PaymentService", "Class"),
        ("PaymentService::charge", "Method"),
        ("Gateway", "Interface"),
        ("LegacyPayment", "Class"),
        ("LegacyPayment::charge", "Method"),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_string(), t.to_string()))
    .collect();
    assert_eq!(entity_set(&ir), want_entities, "exact ts entity set");

    let want_depends: BTreeSet<(String, String)> = [("module", "./kernel"), ("module", "node:os")]
        .into_iter()
        .map(|(s, o)| (s.to_string(), o.to_string()))
        .collect();
    assert_eq!(rel_set(&ir, "depends_on"), want_depends, "exact ts imports");

    let want_impl: BTreeSet<(String, String)> = [
        ("PaymentService", "Gateway"),
        ("LegacyPayment", "PaymentService"),
    ]
    .into_iter()
    .map(|(s, o)| (s.to_string(), o.to_string()))
    .collect();
    assert_eq!(rel_set(&ir, "implements"), want_impl, "exact ts implements");

    let want_tests: BTreeSet<(String, String)> = [
        ("PaymentService", "module"),
        ("charges successfully", "PaymentService"),
    ]
    .into_iter()
    .map(|(s, o)| (s.to_string(), o.to_string()))
    .collect();
    assert_eq!(rel_set(&ir, "tested_by"), want_tests, "exact ts tests");

    let want_facts: BTreeSet<String> = [
        "Payment service implementation.",
        "Charges the given amount.",
        "Legacy charge path.",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(fact_statements(&ir), want_facts, "exact ts doc facts");
}

#[test]
fn a2_java_golden_fixture() {
    let src = r#"import java.util.List;
import com.acme.gateway.Gateway;

/**
 * Payment service implementation.
 */
public class PaymentService implements Gateway {
    /** Charges the given amount. */
    public boolean charge(int amount) {
        return true;
    }
}

/**
 * Legacy payment service.
 */
public class LegacyPayment extends PaymentService {
    /** Legacy charge path. */
    public boolean charge(int amount) {
        return false;
    }
}

public class PaymentServiceTest {
    /** Charge round-trip test. */
    @Test
    public void chargeRoundtrip() {
        assertTrue(true);
    }
}
"#;

    let ir = compile_java_source(src, Some("PaymentService.java"));

    let want_entities: BTreeSet<(String, String)> = [
        ("PaymentService", "Class"),
        ("PaymentService::charge", "Method"),
        ("LegacyPayment", "Class"),
        ("LegacyPayment::charge", "Method"),
        ("PaymentServiceTest", "Class"),
        ("PaymentServiceTest::chargeRoundtrip", "Test"),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_string(), t.to_string()))
    .collect();
    assert_eq!(entity_set(&ir), want_entities, "exact java entity set");

    let want_depends: BTreeSet<(String, String)> = [
        ("module", "java.util.List"),
        ("module", "com.acme.gateway.Gateway"),
    ]
    .into_iter()
    .map(|(s, o)| (s.to_string(), o.to_string()))
    .collect();
    assert_eq!(
        rel_set(&ir, "depends_on"),
        want_depends,
        "exact java imports"
    );

    let want_impl: BTreeSet<(String, String)> = [
        ("PaymentService", "Gateway"),
        ("LegacyPayment", "PaymentService"),
    ]
    .into_iter()
    .map(|(s, o)| (s.to_string(), o.to_string()))
    .collect();
    assert_eq!(
        rel_set(&ir, "implements"),
        want_impl,
        "exact java implements"
    );

    let want_tests: BTreeSet<(String, String)> =
        [("PaymentServiceTest::chargeRoundtrip", "PaymentServiceTest")]
            .into_iter()
            .map(|(s, o)| (s.to_string(), o.to_string()))
            .collect();
    assert_eq!(rel_set(&ir, "tested_by"), want_tests, "exact java tests");

    let want_facts: BTreeSet<String> = [
        "Payment service implementation.",
        "Charges the given amount.",
        "Legacy payment service.",
        "Legacy charge path.",
        "Charge round-trip test.",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(fact_statements(&ir), want_facts, "exact java doc facts");
}
