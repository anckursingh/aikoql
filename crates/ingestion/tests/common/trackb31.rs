//! Wave 3.1 (MVP-QA-003A) market corpus — the task side.
//!
//! 135 new tasks over the 12 W31-MKT-001 workload classes (spec §5),
//! plus the 13 pinned Wave 3 tasks in `trackb.rs` → 148 total. Every
//! task declares the 7 ground-truth fields via `Gt` and every unit is a
//! verbatim corpus sentence or a document id (enforced by
//! `wave31_market.rs`).
//!
//! Class semantics live in docs/market/task-taxonomy.md:
//! - W2 semantic probes share ZERO tokens with their answer units
//!   (lowercased alphanumeric tokens, no stopword filtering — the exact
//!   `common::tokens` contract) — unanswerable by lexical retrieval;
//! - W5 temporal and W6 contradiction and W11 unknown are the spec's
//!   shape classes (≥10% each);
//! - W11 units are TRAPS: real corpus sentences that do not answer the
//!   question. The correct response delivers neither unit.
#![allow(dead_code)]

use super::trackb::{g, Question};

pub const MARKET_QUESTIONS_31: &[Question] = &[
    // ═══ W1 — simple lookup (13) ═══════════════════════════════════════════
    Question {
        text: "How fast does StandardTier respond versus PriorityTier?",
        kind: "lookup",
        class: "W1",
        units: [
            "StandardTier responds within 24 business hours.",
            "PriorityTier responds within 4 hours and escalates to DutyManager.",
        ],
        gt: g("none", "kb-sup-tier", "none", "current", "documentation", "none"),
    },
    Question {
        text: "How fast does PriorityTier respond versus StandardTier?",
        kind: "lookup",
        class: "W1",
        units: [
            "PriorityTier responds within 4 hours and escalates to DutyManager.",
            "StandardTier responds within 24 business hours.",
        ],
        gt: g("none", "kb-sup-tier", "PriorityTier escalates_to DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "Who runs the oncall rotation on Mondays?",
        kind: "lookup",
        class: "W1",
        units: [
            "Alex runs the OncallRotation on Mondays and Wednesdays.",
            "Priya runs the OncallRotation on Tuesdays and Thursdays.",
        ],
        gt: g("none", "kb-oncall", "Alex runs OncallRotation", "current", "documentation", "none"),
    },
    Question {
        text: "Who covers the oncall rotation on weekends?",
        kind: "lookup",
        class: "W1",
        units: [
            "The DutyManager covers the OncallRotation on weekends.",
            "Alex runs the OncallRotation on Mondays and Wednesdays.",
        ],
        gt: g("none", "kb-oncall", "DutyManager covers OncallRotation", "current", "documentation", "none"),
    },
    Question {
        text: "How soon must an IncidentRecord be filed after a breach?",
        kind: "lookup",
        class: "W1",
        units: [
            "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
            "An SLA breach earns customers a 10 percent service credit.",
        ],
        gt: g("none", "kb-sla-credit", "SlaPolicy requires IncidentRecord", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does FraudEngine screen?",
        kind: "lookup",
        class: "W1",
        units: [
            "FraudEngine screens every payout above 10000 dollars.",
            "FraudEngine depends on RiskRules.",
        ],
        gt: g("none", "kb-fraud", "FraudEngine depends_on RiskRules", "current", "documentation", "none"),
    },
    Question {
        text: "When are invoices issued?",
        kind: "lookup",
        class: "W1",
        units: [
            "The BillingCycle issues invoices on the first day of each month.",
            "Overdue invoices enter the DunningFlow on day 15.",
        ],
        gt: g("none", "kb-billing", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What refund applies within 14 days?",
        kind: "lookup",
        class: "W1",
        units: [
            "RefundPolicy grants a full refund within 14 days.",
            "RefundPolicy grants a 50 percent refund between 15 and 30 days.",
        ],
        gt: g("none", "kb-refund", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "What refund applies after 30 days?",
        kind: "lookup",
        class: "W1",
        units: [
            "RefundPolicy grants no refund after 30 days.",
            "RefundPolicy grants a 50 percent refund between 15 and 30 days.",
        ],
        gt: g("none", "kb-refund", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "Who processes card payments and is PCI-DSS certified?",
        kind: "lookup",
        class: "W1",
        units: [
            "ProcessorVendor processes all card payments.",
            "ProcessorVendor is PCI-DSS certified.",
        ],
        gt: g("none", "kb-vendor-payment", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What is the PublicApi rate limit?",
        kind: "lookup",
        class: "W1",
        units: [
            "The PublicApi allows 100 requests per minute per key.",
            "The PublicApi returns a 429 status when the limit is exceeded.",
        ],
        gt: g("none", "kb-api-limits", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When did RefundAutomation ship?",
        kind: "lookup",
        class: "W1",
        units: [
            "RefundAutomation shipped in July.",
            "The Q3Roadmap planned the RefundAutomation feature.",
        ],
        gt: g("none", "kb-roadmap", "Q3Roadmap planned RefundAutomation", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "What does MailVendor handle, and what does CdnVendor provide?",
        kind: "lookup",
        class: "W1",
        units: [
            "MailVendor handles transactional email with a 99 percent SLA.",
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
        ],
        gt: g("none", "kb-vendor-email", "none", "current", "documentation", "none"),
    },

    // ═══ W2 — semantic lookup, zero token overlap (10) ═════════════════════
    Question {
        text: "How many tries are permitted before halting?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "Retry limit is 3 attempts.",
            "PaymentService depends on RetryPolicy.",
        ],
        gt: g("none", "any", "PaymentService depends_on RetryPolicy", "current", "source_code", "none"),
    },
    Question {
        text: "Examination frequency of financial books?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "AuditTeam reviews the ledger each quarter.",
            "Audit activities are mandated by SOX.",
        ],
        gt: g("none", "kb-audit", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When are deploys performed?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "The EngineeringMemo sets the deploy window at midnight.",
            "The SreRunbook sets the deploy window at 6 am.",
        ],
        gt: g("none", "kb-deploy-window", "EngineeringMemo conflicts_with SreRunbook", "current", "documentation", "conflict"),
    },
    Question {
        text: "What happens for enormous transactions?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "FraudEngine screens every payout above 10000 dollars.",
            "FraudEngine depends on RiskRules.",
        ],
        gt: g("none", "kb-fraud", "FraudEngine depends_on RiskRules", "current", "documentation", "none"),
    },
    Question {
        text: "How much downtime is tolerated during releases?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.",
            "The DeployRunbook rolls back the canary if the error rate exceeds 1 percent.",
        ],
        gt: g("none", "kb-runbook-deploy", "none", "current", "documentation", "none"),
    },
    Question {
        text: "Compensation for incidents?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "An SLA breach earns customers a 10 percent service credit.",
            "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
        ],
        gt: g("none", "kb-sla-credit", "SlaPolicy requires IncidentRecord", "current", "organization_policy", "none"),
    },
    Question {
        text: "Capacity permitted per top subscription?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
            "CustomerAlex renewed the ProPlan in May.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "current", "documentation", "none"),
    },
    Question {
        text: "Escalation destination Saturday?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "The DutyManager covers the OncallRotation on weekends.",
            "Alex runs the OncallRotation on Mondays and Wednesdays.",
        ],
        gt: g("none", "kb-oncall", "DutyManager covers OncallRotation", "current", "documentation", "none"),
    },
    Question {
        text: "Customer's tongue for assistance?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "CustomerAlex set the support language preference to Spanish.",
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "current", "documentation", "none"),
    },
    Question {
        text: "Return window duration after buying?",
        kind: "semantic-probe",
        class: "W2",
        units: [
            "RefundPolicy grants a full refund within 14 days.",
            "RefundPolicy grants a 50 percent refund between 15 and 30 days.",
        ],
        gt: g("none", "kb-refund", "none", "current", "organization_policy", "none"),
    },

    // ═══ W3 — multi-source synthesis (11) ═══════════════════════════════════
    Question {
        text: "Which vendor has the highest SLA?",
        kind: "synthesis",
        class: "W3",
        units: [
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
            "MailVendor handles transactional email with a 99 percent SLA.",
        ],
        gt: g("none", "any", "CdnVendor uses EdgeNodes", "current", "documentation", "none"),
    },
    Question {
        text: "What did the CDN vendor fail, and what does it owe?",
        kind: "synthesis",
        class: "W3",
        units: [
            "CdnVendor failed its SLA review in March.",
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
        ],
        gt: g("none", "any", "CdnVendor uses EdgeNodes", "mixed", "documentation", "none"),
    },
    Question {
        text: "What must a new vendor provide, and how is compliance verified?",
        kind: "synthesis",
        class: "W3",
        units: [
            "ProcurementRule requires two vendor bids and a SecurityReview.",
            "ProcessorVendor is PCI-DSS certified.",
        ],
        gt: g("none", "any", "ProcurementRule requires SecurityReview", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does the fraud screening do, and what rules drive it?",
        kind: "synthesis",
        class: "W3",
        units: [
            "FraudEngine screens every payout above 10000 dollars.",
            "RiskRules block accounts with more than 50 payouts per day.",
        ],
        gt: g("none", "any", "FraudEngine depends_on RiskRules", "current", "documentation", "none"),
    },
    Question {
        text: "What support do premium customers get, and who handles the worst cases?",
        kind: "synthesis",
        class: "W3",
        units: [
            "PriorityTier responds within 4 hours and escalates to DutyManager.",
            "The DutyManager covers the OncallRotation on weekends.",
        ],
        gt: g("none", "any", "PriorityTier escalates_to DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "What is the payout limit policy, and who enforces it?",
        kind: "synthesis",
        class: "W3",
        units: [
            "FraudEngine screens every payout above 10000 dollars.",
            "RiskRules updates the country blocklist every quarter.",
        ],
        gt: g("none", "any", "FraudEngine depends_on RiskRules", "current", "documentation", "none"),
    },
    Question {
        text: "What did the outage do to billing, and what was the fix?",
        kind: "synthesis",
        class: "W3",
        units: [
            "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.",
            "ArchV3 replaced ArchV2 in June and is the current architecture.",
        ],
        gt: g("none", "any", "ArchV3 replaced ArchV2", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "How does the catalog protect PII and who must answer DSARs?",
        kind: "synthesis",
        class: "W3",
        units: [
            "UsersDB encrypts PII at rest.",
            "DsarPolicy requires answering subject access requests within 30 days.",
        ],
        gt: g("none", "any", "DataCatalog maps UsersDB", "current", "organization_policy", "none"),
    },
    Question {
        text: "Which vendors failed reviews, and what must customers receive after breaches?",
        kind: "synthesis",
        class: "W3",
        units: [
            "CdnVendor failed its SLA review in March.",
            "An SLA breach earns customers a 10 percent service credit.",
        ],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "What does the procurement rule demand, and how often are vendors reviewed?",
        kind: "synthesis",
        class: "W3",
        units: [
            "ProcurementRule requires two vendor bids and a SecurityReview.",
            "VendorSlaPolicy requires quarterly SLA reviews for every vendor.",
        ],
        gt: g("none", "any", "ProcurementRule requires SecurityReview", "current", "organization_policy", "none"),
    },
    Question {
        text: "What changed in April, and what effect did it have?",
        kind: "synthesis",
        class: "W3",
        units: [
            "CheckoutService gained the OneClick feature in April.",
            "The OneClick feature increased checkout conversion by 12 percent.",
        ],
        gt: g("none", "kb-changelog", "none", "mixed", "deployment_observed", "none"),
    },

    // ═══ W4 — multi-hop reasoning (9) ═══════════════════════════════════════
    Question {
        text: "What must HighRisk accounts complete before the first payout?",
        kind: "hop",
        class: "W4",
        units: [
            "HighRisk accounts must complete KycPolicy verification before the first payout.",
            "KycPolicy requires government ID and proof of address within 30 days.",
        ],
        gt: g("none", "kb-kyc", "HighRisk requires KycPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "Where does the catalog say PII lives?",
        kind: "hop",
        class: "W4",
        units: [
            "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
            "UsersDB runs nightly integrity checks.",
        ],
        gt: g("none", "any", "DataCatalog maps UsersDB", "current", "documentation", "none"),
    },
    Question {
        text: "Who runs the rotation on Mondays, and who covers weekends?",
        kind: "hop",
        class: "W4",
        units: [
            "Alex runs the OncallRotation on Mondays and Wednesdays.",
            "The DutyManager covers the OncallRotation on weekends.",
        ],
        gt: g("none", "kb-oncall", "Alex runs OncallRotation", "current", "documentation", "none"),
    },
    Question {
        text: "What must be filed when an SLA breach occurs?",
        kind: "hop",
        class: "W4",
        units: [
            "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
            "An SLA breach earns customers a 10 percent service credit.",
        ],
        gt: g("none", "kb-sla-credit", "SlaPolicy requires IncidentRecord", "current", "organization_policy", "none"),
    },
    Question {
        text: "What governs the payout screens?",
        kind: "cross-doc",
        class: "W4",
        units: [
            "FraudEngine depends on RiskRules.",
            "RiskRules block accounts with more than 50 payouts per day.",
        ],
        gt: g("none", "any", "FraudEngine depends_on RiskRules", "current", "documentation", "none"),
    },
    Question {
        text: "Who gets paged for a Sev1?",
        kind: "cross-doc",
        class: "W4",
        units: [
            "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
            "The DutyManager covers the OncallRotation on weekends.",
        ],
        gt: g("none", "any", "Sev1Runbook pages DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "Where do PriorityTier issues escalate?",
        kind: "cross-doc",
        class: "W4",
        units: [
            "PriorityTier responds within 4 hours and escalates to DutyManager.",
            "The DutyManager covers the OncallRotation on weekends.",
        ],
        gt: g("none", "any", "PriorityTier escalates_to DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "When does the dunning flow start and what does it do?",
        kind: "cross-doc",
        class: "W4",
        units: [
            "Overdue invoices enter the DunningFlow on day 15.",
            "The DunningFlow sends a warning on day 22 and suspends the account on day 30.",
        ],
        gt: g("none", "any", "none", "current", "documentation", "none"),
    },
    Question {
        text: "How is the PII in UsersDB protected?",
        kind: "cross-doc",
        class: "W4",
        units: [
            "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
            "UsersDB encrypts PII at rest.",
        ],
        gt: g("none", "any", "DataCatalog maps UsersDB", "current", "documentation", "none"),
    },

    // ═══ W5 — temporal reasoning (14) ═══════════════════════════════════════
    Question {
        text: "What is the current retention period?",
        kind: "cross-doc",
        class: "W5",
        units: [
            "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.",
            "RetentionV1 keeps user records for 12 months.",
        ],
        gt: g("none", "any", "RetentionV2 replaced RetentionV1", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "How long were user records kept before May?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "RetentionV1 keeps user records for 12 months.",
            "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.",
        ],
        gt: g("none", "any", "RetentionV2 replaced RetentionV1", "historical", "organization_policy", "none"),
    },
    Question {
        text: "When did RetentionV2 take effect?",
        kind: "cross-doc",
        class: "W5",
        units: [
            "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.",
            "RetentionV1 keeps user records for 12 months.",
        ],
        gt: g("none", "any", "RetentionV2 replaced RetentionV1", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "What is the current service credit for a breach?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "SlaPolicy raised the service credit from 5 percent to 10 percent in April.",
            "An SLA breach earns customers a 10 percent service credit.",
        ],
        gt: g("none", "any", "none", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "What service credit applied before April?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "SlaPolicy provided a 5 percent credit before April.",
            "SlaPolicy raised the service credit from 5 percent to 10 percent in April.",
        ],
        gt: g("none", "kb-sla-change", "none", "historical", "organization_policy", "none"),
    },
    Question {
        text: "When did SlaPolicy change?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "SlaPolicy raised the service credit from 5 percent to 10 percent in April.",
            "SlaPolicy provided a 5 percent credit before April.",
        ],
        gt: g("none", "kb-sla-change", "none", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "What is the current payout screening threshold?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.",
            "FraudEngine screens every payout above 10000 dollars.",
        ],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "What threshold applied before February?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "RiskRules screened payouts above 20000 dollars before February.",
            "RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.",
        ],
        gt: g("none", "kb-risk-change", "none", "historical", "documentation", "none"),
    },
    Question {
        text: "What happened to CustomerPriya's plan in September?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
            "CustomerPriya subscribed to the BasicPlan with 2 seats.",
        ],
        gt: g("none", "kb-cust-priya", "CustomerPriya upgraded_from BasicPlan", "mixed", "documentation", "none"),
    },
    Question {
        text: "What SLA event happened in March?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "CdnVendor failed its SLA review in March.",
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
        ],
        gt: g("none", "any", "CdnVendor uses EdgeNodes", "mixed", "documentation", "none"),
    },
    Question {
        text: "What changed for CheckoutService in April?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "CheckoutService gained the OneClick feature in April.",
            "The OneClick feature increased checkout conversion by 12 percent.",
        ],
        gt: g("none", "kb-changelog", "none", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "What is the current architecture?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "ArchV3 replaced ArchV2 in June and is the current architecture.",
            "The rollout of ArchV3 fixed the FebruaryOutage for BillingCustomers.",
        ],
        gt: g("none", "any", "ArchV3 replaced ArchV2", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "What was the active architecture during the FebruaryOutage?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "The FebruaryOutage hit BillingCustomers while ArchV1 was the active architecture.",
            "ArchV1 handled payments from January through February.",
        ],
        gt: g("none", "any", "none", "historical", "deployment_observed", "none"),
    },
    Question {
        text: "What did ArchV2 add in March?",
        kind: "temporal-probe",
        class: "W5",
        units: [
            "ArchV2 replaced ArchV1 in March and added the retry cache.",
            "ArchV1 handled payments from January through February.",
        ],
        gt: g("none", "any", "ArchV2 replaced ArchV1", "mixed", "deployment_observed", "none"),
    },

    // ═══ W6 — contradiction resolution (14) ═════════════════════════════════
    Question {
        text: "When is the deploy window per the EngineeringMemo?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The EngineeringMemo sets the deploy window at midnight.",
            "The SreRunbook sets the deploy window at 6 am.",
        ],
        gt: g("either documented time; both sources present", "kb-deploy-window", "EngineeringMemo conflicts_with SreRunbook", "current", "documentation", "conflict"),
    },
    Question {
        text: "When is the deploy window per the SreRunbook?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SreRunbook sets the deploy window at 6 am.",
            "The EngineeringMemo sets the deploy window at midnight.",
        ],
        gt: g("either documented time; both sources present", "kb-deploy-window", "EngineeringMemo conflicts_with SreRunbook", "current", "documentation", "conflict"),
    },
    Question {
        text: "What do the deploy window documents disagree on?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The EngineeringMemo conflicts with the SreRunbook.",
            "The EngineeringMemo sets the deploy window at midnight.",
        ],
        gt: g("none", "kb-deploy-window", "EngineeringMemo conflicts_with SreRunbook", "current", "documentation", "conflict"),
    },
    Question {
        text: "What database does the architecture decision mandate?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The DbArchitectureDecision mandates Postgres as the primary database.",
            "The PilotProposal proposes MySQL for the analytics workload.",
        ],
        gt: g("either proposed database; both sources present", "kb-db-choice", "DbArchitectureDecision conflicts_with PilotProposal", "current", "documentation", "conflict"),
    },
    Question {
        text: "What does the pilot proposal propose?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The PilotProposal proposes MySQL for the analytics workload.",
            "The DbArchitectureDecision mandates Postgres as the primary database.",
        ],
        gt: g("either proposed database; both sources present", "kb-db-choice", "DbArchitectureDecision conflicts_with PilotProposal", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which documents conflict on the database choice?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The DbArchitectureDecision conflicts with the PilotProposal.",
            "The DbArchitectureDecision mandates Postgres as the primary database.",
        ],
        gt: g("none", "kb-db-choice", "DbArchitectureDecision conflicts_with PilotProposal", "current", "documentation", "conflict"),
    },
    Question {
        text: "What hours does the SupportPage list?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SupportPage lists support hours as 9 to 5 weekdays.",
            "The InternalWiki lists support hours as 8 to 6 weekdays.",
        ],
        gt: g("either documented schedule; both sources present", "kb-support-hours", "SupportPage conflicts_with InternalWiki", "current", "documentation", "conflict"),
    },
    Question {
        text: "What hours does the InternalWiki list?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The InternalWiki lists support hours as 8 to 6 weekdays.",
            "The SupportPage lists support hours as 9 to 5 weekdays.",
        ],
        gt: g("either documented schedule; both sources present", "kb-support-hours", "SupportPage conflicts_with InternalWiki", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which pages conflict on support hours?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SupportPage conflicts with the InternalWiki on support hours.",
            "The SupportPage lists support hours as 9 to 5 weekdays.",
        ],
        gt: g("none", "kb-support-hours", "SupportPage conflicts_with InternalWiki", "current", "documentation", "conflict"),
    },
    Question {
        text: "When does Alex run oncall per the SchedulingMemo?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SchedulingMemo says Alex runs Mondays.",
            "The TeamWiki says Alex runs Tuesdays.",
        ],
        gt: g("either documented day; both sources present", "kb-oncall-conflict", "SchedulingMemo conflicts_with TeamWiki", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which documents disagree about Alex's schedule?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SchedulingMemo conflicts with the TeamWiki.",
            "The SchedulingMemo says Alex runs Mondays.",
        ],
        gt: g("none", "kb-oncall-conflict", "SchedulingMemo conflicts_with TeamWiki", "current", "documentation", "conflict"),
    },
    Question {
        text: "What does the SupportFaQ claim about refunds?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SupportFaQ says refunds apply within 30 days.",
            "RefundPolicy grants no refund after 30 days.",
        ],
        gt: g("either documented refund window; both sources present", "any", "SupportFaQ conflicts_with RefundPolicy", "current", "documentation", "conflict"),
    },
    Question {
        text: "What does RefundPolicy say after 30 days?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "RefundPolicy grants no refund after 30 days.",
            "The SupportFaQ says refunds apply within 30 days.",
        ],
        gt: g("either documented refund window; both sources present", "any", "SupportFaQ conflicts_with RefundPolicy", "current", "documentation", "conflict"),
    },
    Question {
        text: "Which documents conflict on refunds?",
        kind: "contradiction-probe",
        class: "W6",
        units: [
            "The SupportFaQ conflicts with RefundPolicy.",
            "The SupportFaQ says refunds apply within 30 days.",
        ],
        gt: g("none", "any", "SupportFaQ conflicts_with RefundPolicy", "current", "documentation", "conflict"),
    },

    // ═══ W7 — provenance / evidence (9) ═════════════════════════════════════
    Question {
        text: "Which document certifies the CDN SLA?",
        kind: "provenance",
        class: "W7",
        units: [
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
            "kb-vendor-cdn",
        ],
        gt: g("none", "kb-vendor-cdn", "CdnVendor uses EdgeNodes", "current", "documentation", "none"),
    },
    Question {
        text: "Which document certifies the payment processor?",
        kind: "provenance",
        class: "W7",
        units: [
            "ProcessorVendor is PCI-DSS certified.",
            "kb-vendor-payment",
        ],
        gt: g("none", "kb-vendor-payment", "none", "current", "documentation", "none"),
    },
    Question {
        text: "Where does the 24-hour response time come from?",
        kind: "provenance",
        class: "W7",
        units: [
            "StandardTier responds within 24 business hours.",
            "kb-sup-tier",
        ],
        gt: g("none", "kb-sup-tier", "none", "current", "documentation", "none"),
    },
    Question {
        text: "Which document defines the KYC requirements?",
        kind: "provenance",
        class: "W7",
        units: [
            "KycPolicy requires government ID and proof of address within 30 days.",
            "kb-kyc",
        ],
        gt: g("none", "kb-kyc", "HighRisk requires KycPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "Which document sets the quarterly vendor review?",
        kind: "provenance",
        class: "W7",
        units: [
            "VendorSlaPolicy requires quarterly SLA reviews for every vendor.",
            "kb-vendor-sla",
        ],
        gt: g("none", "kb-vendor-sla", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "Where is the retention period documented?",
        kind: "provenance",
        class: "W7",
        units: [
            "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.",
            "kb-retention-v2",
        ],
        gt: g("none", "kb-retention-v2", "RetentionV2 replaced RetentionV1", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "Which document covers the CDN sourcing?",
        kind: "provenance",
        class: "W7",
        units: [
            "CdnVendor sources capacity from EdgeNodes.",
            "kb-vendor-cdn",
        ],
        gt: g("none", "kb-vendor-cdn", "CdnVendor uses EdgeNodes", "current", "documentation", "none"),
    },
    Question {
        text: "Where does the DSAR deadline come from?",
        kind: "provenance",
        class: "W7",
        units: [
            "DsarPolicy requires answering subject access requests within 30 days.",
            "kb-dsar",
        ],
        gt: g("none", "kb-dsar", "DataCatalog maps UsersDB", "current", "organization_policy", "none"),
    },
    Question {
        text: "Which document documents the Sev1 flow?",
        kind: "provenance",
        class: "W7",
        units: [
            "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
            "kb-runbook-sev1",
        ],
        gt: g("none", "kb-runbook-sev1", "Sev1Runbook pages DutyManager", "current", "documentation", "none"),
    },

    // ═══ W8 — persistent memory (10) ════════════════════════════════════════
    Question {
        text: "What plan does CustomerAlex have?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
            "CustomerAlex renewed the ProPlan in May.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "current", "documentation", "none"),
    },
    Question {
        text: "How many seats does CustomerAlex have?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
            "CustomerAlex set the support language preference to Spanish.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "current", "documentation", "none"),
    },
    Question {
        text: "What language did CustomerAlex set?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerAlex set the support language preference to Spanish.",
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
        ],
        gt: g("none", "kb-cust-alex", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When did CustomerAlex renew?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerAlex renewed the ProPlan in May.",
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "mixed", "documentation", "none"),
    },
    Question {
        text: "What plan does CustomerPriya have now?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
            "CustomerPriya subscribed to the BasicPlan with 2 seats.",
        ],
        gt: g("none", "kb-cust-priya", "CustomerPriya upgraded_from BasicPlan", "mixed", "documentation", "none"),
    },
    Question {
        text: "What plan did CustomerPriya start with?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerPriya subscribed to the BasicPlan with 2 seats.",
            "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
        ],
        gt: g("none", "kb-cust-priya", "CustomerPriya upgraded_from BasicPlan", "historical", "documentation", "none"),
    },
    Question {
        text: "What language does CustomerPriya prefer?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerPriya set the support language preference to French.",
            "CustomerPriya subscribed to the BasicPlan with 2 seats.",
        ],
        gt: g("none", "kb-cust-priya", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What plan does CustomerDev have?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerDev subscribed to the ProPlan with 12 seats.",
            "CustomerDev renewed the ProPlan in June.",
        ],
        gt: g("none", "kb-cust-dev", "CustomerDev subscribed_to ProPlan", "current", "documentation", "none"),
    },
    Question {
        text: "What email preference does CustomerDev have?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerDev prefers weekly summary emails.",
            "CustomerDev subscribed to the ProPlan with 12 seats.",
        ],
        gt: g("none", "kb-cust-dev", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When did CustomerDev renew?",
        kind: "personal",
        class: "W8",
        units: [
            "CustomerDev renewed the ProPlan in June.",
            "CustomerDev subscribed to the ProPlan with 12 seats.",
        ],
        gt: g("none", "kb-cust-dev", "CustomerDev subscribed_to ProPlan", "mixed", "documentation", "none"),
    },

    // ═══ W9 — policy / constraint reasoning (11) ════════════════════════════
    Question {
        text: "What does KycPolicy require?",
        kind: "policy",
        class: "W9",
        units: [
            "KycPolicy requires government ID and proof of address within 30 days.",
            "HighRisk accounts must complete KycPolicy verification before the first payout.",
        ],
        gt: g("none", "kb-kyc", "HighRisk requires KycPolicy", "current", "organization_policy", "none"),
    },
    Question {
        text: "What do customers get when an SLA is breached?",
        kind: "policy",
        class: "W9",
        units: [
            "An SLA breach earns customers a 10 percent service credit.",
            "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
        ],
        gt: g("none", "kb-sla-credit", "SlaPolicy requires IncidentRecord", "current", "organization_policy", "none"),
    },
    Question {
        text: "What do RiskRules block?",
        kind: "policy",
        class: "W9",
        units: [
            "RiskRules block accounts with more than 50 payouts per day.",
            "RiskRules updates the country blocklist every quarter.",
        ],
        gt: g("none", "kb-risk-rules", "none", "current", "documentation", "none"),
    },
    Question {
        text: "How often does the country blocklist update?",
        kind: "policy",
        class: "W9",
        units: [
            "RiskRules updates the country blocklist every quarter.",
            "RiskRules block accounts with more than 50 payouts per day.",
        ],
        gt: g("none", "kb-risk-rules", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When do overdue invoices enter the dunning flow?",
        kind: "policy",
        class: "W9",
        units: [
            "Overdue invoices enter the DunningFlow on day 15.",
            "The BillingCycle issues invoices on the first day of each month.",
        ],
        gt: g("none", "kb-billing", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What refund applies between 15 and 30 days?",
        kind: "policy",
        class: "W9",
        units: [
            "RefundPolicy grants a 50 percent refund between 15 and 30 days.",
            "RefundPolicy grants no refund after 30 days.",
        ],
        gt: g("none", "kb-refund", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does ProcurementRule require?",
        kind: "policy",
        class: "W9",
        units: [
            "ProcurementRule requires two vendor bids and a SecurityReview.",
            "The SecurityReview checks vendor access to card data.",
        ],
        gt: g("none", "kb-procurement", "ProcurementRule requires SecurityReview", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does DsarPolicy require?",
        kind: "policy",
        class: "W9",
        units: [
            "DsarPolicy requires answering subject access requests within 30 days.",
            "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
        ],
        gt: g("none", "kb-dsar", "DataCatalog maps UsersDB", "current", "organization_policy", "none"),
    },
    Question {
        text: "How often are vendor SLA reviews required?",
        kind: "policy",
        class: "W9",
        units: [
            "VendorSlaPolicy requires quarterly SLA reviews for every vendor.",
            "CdnVendor failed its SLA review in March.",
        ],
        gt: g("none", "kb-vendor-sla", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "What happens when the API limit is exceeded?",
        kind: "policy",
        class: "W9",
        units: [
            "The PublicApi returns a 429 status when the limit is exceeded.",
            "The PublicApi allows 100 requests per minute per key.",
        ],
        gt: g("none", "kb-api-limits", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What does the security review check?",
        kind: "policy",
        class: "W9",
        units: [
            "The SecurityReview checks vendor access to card data.",
            "ProcurementRule requires two vendor bids and a SecurityReview.",
        ],
        gt: g("none", "kb-procurement", "ProcurementRule requires SecurityReview", "current", "organization_policy", "none"),
    },

    // ═══ W10 — agent planning (10) ══════════════════════════════════════════
    Question {
        text: "What are the deploy runbook steps?",
        kind: "planning",
        class: "W10",
        units: [
            "The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.",
            "The DeployRunbook rolls back the canary if the error rate exceeds 1 percent.",
        ],
        gt: g("none", "kb-runbook-deploy", "none", "current", "documentation", "none"),
    },
    Question {
        text: "When does the deploy runbook roll back?",
        kind: "planning",
        class: "W10",
        units: [
            "The DeployRunbook rolls back the canary if the error rate exceeds 1 percent.",
            "The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.",
        ],
        gt: g("none", "kb-runbook-deploy", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What are the database failover steps?",
        kind: "planning",
        class: "W10",
        units: [
            "The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.",
            "The DbFailoverRunbook files an IncidentRecord after every failover.",
        ],
        gt: g("none", "kb-runbook-db", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What happens after every database failover?",
        kind: "planning",
        class: "W10",
        units: [
            "The DbFailoverRunbook files an IncidentRecord after every failover.",
            "The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.",
        ],
        gt: g("none", "kb-runbook-db", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What are the Sev1 response steps?",
        kind: "planning",
        class: "W10",
        units: [
            "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
            "The Sev1Runbook requires a postmortem within 5 business days.",
        ],
        gt: g("none", "kb-runbook-sev1", "Sev1Runbook pages DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "When is the postmortem due?",
        kind: "planning",
        class: "W10",
        units: [
            "The Sev1Runbook requires a postmortem within 5 business days.",
            "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
        ],
        gt: g("none", "kb-runbook-sev1", "Sev1Runbook pages DutyManager", "current", "documentation", "none"),
    },
    Question {
        text: "What are the onboarding steps?",
        kind: "planning",
        class: "W10",
        units: [
            "The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account.",
            "KycPolicy requires government ID and proof of address within 30 days.",
        ],
        gt: g("none", "any", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "What does onboarding verify first?",
        kind: "planning",
        class: "W10",
        units: [
            "The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account.",
            "KycPolicy requires government ID and proof of address within 30 days.",
        ],
        gt: g("none", "any", "none", "current", "organization_policy", "none"),
    },
    Question {
        text: "What are the dunning flow steps?",
        kind: "planning",
        class: "W10",
        units: [
            "The DunningFlow sends a reminder on day 15.",
            "The DunningFlow sends a warning on day 22 and suspends the account on day 30.",
        ],
        gt: g("none", "kb-dunning", "none", "current", "documentation", "none"),
    },
    Question {
        text: "What happens on day 22?",
        kind: "planning",
        class: "W10",
        units: [
            "The DunningFlow sends a warning on day 22 and suspends the account on day 30.",
            "The DunningFlow sends a reminder on day 15.",
        ],
        gt: g("none", "kb-dunning", "none", "current", "documentation", "none"),
    },

    // ═══ W11 — unknown / insufficient evidence (14, trap units) ════════════
    Question {
        text: "What is the disaster recovery site?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The DrPlan document is still under review.",
            "The Sev1Runbook requires a postmortem within 5 business days.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "When does the ProPlan price increase?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "CustomerAlex renewed the ProPlan in May.",
            "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "Who owns the Terraform module registry?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
            "UsersDB runs nightly integrity checks.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "What is the end-of-life date for ArchV2?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "ArchV2 replaced ArchV1 in March and added the retry cache.",
            "ArchV3 replaced ArchV2 in June and is the current architecture.",
        ],
        gt: g("no authoritative answer", "any", "ArchV3 replaced ArchV2", "current", "deployment_observed", "unknown"),
    },
    Question {
        text: "Which SSO provider does the platform use?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "ProcessorVendor is PCI-DSS certified.",
            "MailVendor handles transactional email with a 99 percent SLA.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "What is the backup recovery time objective?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.",
            "UsersDB runs nightly integrity checks.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "Is there an on-premises offering?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "CdnVendor sources capacity from EdgeNodes.",
            "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
        ],
        gt: g("no authoritative answer", "any", "CdnVendor uses EdgeNodes", "current", "documentation", "unknown"),
    },
    Question {
        text: "How do customers export their data?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "DsarPolicy requires answering subject access requests within 30 days.",
            "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
        ],
        gt: g("no authoritative answer", "any", "DataCatalog maps UsersDB", "current", "organization_policy", "unknown"),
    },
    Question {
        text: "When does the legacy API sunset?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The PublicApi allows 100 requests per minute per key.",
            "The PublicApi returns a 429 status when the limit is exceeded.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "Which log aggregator is used?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The DbFailoverRunbook files an IncidentRecord after every failover.",
            "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
        ],
        gt: g("no authoritative answer", "any", "none", "current", "documentation", "unknown"),
    },
    Question {
        text: "What is the staging environment URL?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The EngineeringMemo sets the deploy window at midnight.",
            "The SreRunbook sets the deploy window at 6 am.",
        ],
        gt: g("no authoritative answer", "any", "EngineeringMemo conflicts_with SreRunbook", "current", "documentation", "unknown"),
    },
    Question {
        text: "Who is the security officer?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The SecurityReview checks vendor access to card data.",
            "ProcurementRule requires two vendor bids and a SecurityReview.",
        ],
        gt: g("no authoritative answer", "any", "ProcurementRule requires SecurityReview", "current", "organization_policy", "unknown"),
    },
    Question {
        text: "What is the incident simulator tool?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
            "The DutyManager covers the OncallRotation on weekends.",
        ],
        gt: g("no authoritative answer", "any", "Sev1Runbook pages DutyManager", "current", "documentation", "unknown"),
    },
    Question {
        text: "When is the next SOC2 audit?",
        kind: "unknown-probe",
        class: "W11",
        units: [
            "KycPolicy requires government ID and proof of address within 30 days.",
            "HighRisk accounts must complete KycPolicy verification before the first payout.",
        ],
        gt: g("no authoritative answer", "any", "HighRisk requires KycPolicy", "current", "organization_policy", "unknown"),
    },

    // ═══ W12 — longitudinal evolution (10) ══════════════════════════════════
    Question {
        text: "What did the Q3 roadmap plan?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "The Q3Roadmap planned the RefundAutomation feature.",
            "The Q4Roadmap plans the MultiCurrency feature.",
        ],
        gt: g("none", "kb-roadmap", "Q3Roadmap planned RefundAutomation", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "What shipped from the Q3 roadmap?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "RefundAutomation shipped in July.",
            "The Q3Roadmap planned the RefundAutomation feature.",
        ],
        gt: g("none", "kb-roadmap", "Q3Roadmap planned RefundAutomation", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "What is planned for Q4?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "The Q4Roadmap plans the MultiCurrency feature.",
            "RefundAutomation shipped in July.",
        ],
        gt: g("none", "kb-roadmap", "Q4Roadmap plans MultiCurrency", "current", "deployment_observed", "none"),
    },
    Question {
        text: "How did the OneClick feature perform?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "The OneClick feature increased checkout conversion by 12 percent.",
            "CheckoutService gained the OneClick feature in April.",
        ],
        gt: g("none", "kb-changelog", "none", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "How did CustomerPriya's subscription evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
            "CustomerPriya subscribed to the BasicPlan with 2 seats.",
        ],
        gt: g("none", "kb-cust-priya", "CustomerPriya upgraded_from BasicPlan", "mixed", "documentation", "none"),
    },
    Question {
        text: "How did the retention policy evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.",
            "RetentionV1 keeps user records for 12 months.",
        ],
        gt: g("none", "any", "RetentionV2 replaced RetentionV1", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "How did the SLA credit evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "SlaPolicy raised the service credit from 5 percent to 10 percent in April.",
            "An SLA breach earns customers a 10 percent service credit.",
        ],
        gt: g("none", "any", "none", "mixed", "organization_policy", "none"),
    },
    Question {
        text: "How did the risk threshold evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.",
            "FraudEngine screens every payout above 10000 dollars.",
        ],
        gt: g("none", "any", "none", "mixed", "documentation", "none"),
    },
    Question {
        text: "How did the architecture evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "ArchV3 replaced ArchV2 in June and is the current architecture.",
            "ArchV1 handled payments from January through February.",
        ],
        gt: g("none", "any", "ArchV3 replaced ArchV2", "mixed", "deployment_observed", "none"),
    },
    Question {
        text: "How did CustomerAlex's subscription evolve?",
        kind: "longitudinal",
        class: "W12",
        units: [
            "CustomerAlex renewed the ProPlan in May.",
            "CustomerAlex subscribed to the ProPlan with 5 seats.",
        ],
        gt: g("none", "kb-cust-alex", "CustomerAlex subscribed_to ProPlan", "mixed", "documentation", "none"),
    },
];
