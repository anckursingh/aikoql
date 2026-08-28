//! Wave 3.1 (MVP-QA-003A) market corpus — the document side.
//!
//! The Wave 3 market corpus was 19 documents / 34 chunks / 13 questions.
//! W31-MKT-001 (spec §5) expands it into a frozen ≥100-task corpus.
//! These 36 synthetic support/finance/compliance/ops/customer documents
//! hold the ground truth; `trackb31.rs` holds the questions. Every fact
//! is an exact chunk sentence, every entity name appears in a chunk, and
//! every relation's endpoints are co-mentioned in a chunk — the same
//! integrity contract as `trackb.rs` (`assert_integrity` enforces it).
//!
//! Design notes (documented in docs/market/corpus-version.md):
//! - contradictions are recorded explicitly (a "X conflicts with Y" fact)
//!   so the corpus states the disagreement instead of the builder
//!   adjudicating it;
//! - temporal change pairs (retention, SLA credit, risk threshold,
//!   architecture) carry both the superseding and the historical fact;
//! - the customer entities are named CustomerAlex/CustomerPriya/CustomerDev
//!   so they never merge with the Alex/Priya engineer entities in
//!   kb-oncall;
//! - kb-dr-plan is a deliberate trap: a document exists, but it does not
//!   answer the disaster-recovery questions.
#![allow(dead_code)]

use aikoql_ingestion::{EntityCandidate, Evidence, FactCandidate, KnowledgeIr, RelationCandidate};

use super::trackb::Doc;

fn ev(doc: &str) -> Evidence {
    Evidence {
        document_id: Some(doc.into()),
        extractor: "w31-market-synthetic".into(),
        confidence: 0.9,
        ..Evidence::default()
    }
}

fn entity(name: &str, ty: &str, mention: &str, doc: &str) -> EntityCandidate {
    EntityCandidate {
        name: name.into(),
        type_hint: Some(ty.into()),
        mentions: vec![mention.into()],
        confidence: 0.9,
        evidence: ev(doc),
    }
}

fn fact(statement: &str, anchors: &[&str], doc: &str) -> FactCandidate {
    FactCandidate {
        statement: statement.into(),
        entities: anchors.iter().map(|s| s.to_string()).collect(),
        confidence: 0.9,
        evidence: ev(doc),
        snippet: None,
    }
}

fn rel(subject: &str, predicate: &str, object: &str, doc: &str) -> RelationCandidate {
    RelationCandidate {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        confidence: 0.9,
        evidence: ev(doc),
    }
}

pub fn market_docs_31() -> Vec<Doc> {
    vec![
        Doc {
            id: "kb-sup-tier",
            chunks: &[
                "StandardTier responds within 24 business hours.",
                "PriorityTier responds within 4 hours and escalates to DutyManager.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("StandardTier", "Tier", "StandardTier responds within 24 business hours.", "kb-sup-tier"),
                    entity("PriorityTier", "Tier", "PriorityTier responds within 4 hours and escalates to DutyManager.", "kb-sup-tier"),
                    entity("DutyManager", "Team", "PriorityTier responds within 4 hours and escalates to DutyManager.", "kb-sup-tier"),
                ],
                facts: vec![
                    fact("StandardTier responds within 24 business hours.", &["StandardTier"], "kb-sup-tier"),
                    fact("PriorityTier responds within 4 hours and escalates to DutyManager.", &["PriorityTier"], "kb-sup-tier"),
                ],
                relations: vec![rel("PriorityTier", "escalates_to", "DutyManager", "kb-sup-tier")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-sla-credit",
            chunks: &[
                "An SLA breach earns customers a 10 percent service credit.",
                "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("SlaPolicy", "Policy", "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.", "kb-sla-credit"),
                    entity("IncidentRecord", "Record", "SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.", "kb-sla-credit"),
                ],
                facts: vec![
                    fact("An SLA breach earns customers a 10 percent service credit.", &["SlaPolicy"], "kb-sla-credit"),
                    fact("SlaPolicy requires filing an IncidentRecord within 30 minutes of a breach.", &["SlaPolicy"], "kb-sla-credit"),
                ],
                relations: vec![rel("SlaPolicy", "requires", "IncidentRecord", "kb-sla-credit")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-sla-change",
            chunks: &[
                "SlaPolicy raised the service credit from 5 percent to 10 percent in April.",
                "SlaPolicy provided a 5 percent credit before April.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("SlaPolicy", "Policy", "SlaPolicy raised the service credit from 5 percent to 10 percent in April.", "kb-sla-change"),
                ],
                facts: vec![
                    fact("SlaPolicy raised the service credit from 5 percent to 10 percent in April.", &["SlaPolicy"], "kb-sla-change"),
                    fact("SlaPolicy provided a 5 percent credit before April.", &["SlaPolicy"], "kb-sla-change"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-oncall",
            chunks: &[
                "Alex runs the OncallRotation on Mondays and Wednesdays.",
                "Priya runs the OncallRotation on Tuesdays and Thursdays.",
                "The DutyManager covers the OncallRotation on weekends.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("Alex", "Engineer", "Alex runs the OncallRotation on Mondays and Wednesdays.", "kb-oncall"),
                    entity("Priya", "Engineer", "Priya runs the OncallRotation on Tuesdays and Thursdays.", "kb-oncall"),
                    entity("DutyManager", "Team", "The DutyManager covers the OncallRotation on weekends.", "kb-oncall"),
                    entity("OncallRotation", "Process", "Alex runs the OncallRotation on Mondays and Wednesdays.", "kb-oncall"),
                ],
                facts: vec![
                    fact("Alex runs the OncallRotation on Mondays and Wednesdays.", &["Alex"], "kb-oncall"),
                    fact("Priya runs the OncallRotation on Tuesdays and Thursdays.", &["Priya"], "kb-oncall"),
                    fact("The DutyManager covers the OncallRotation on weekends.", &["DutyManager"], "kb-oncall"),
                ],
                relations: vec![
                    rel("Alex", "runs", "OncallRotation", "kb-oncall"),
                    rel("Priya", "runs", "OncallRotation", "kb-oncall"),
                    rel("DutyManager", "covers", "OncallRotation", "kb-oncall"),
                ],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-kyc",
            chunks: &[
                "KycPolicy requires government ID and proof of address within 30 days.",
                "HighRisk accounts must complete KycPolicy verification before the first payout.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("KycPolicy", "Policy", "KycPolicy requires government ID and proof of address within 30 days.", "kb-kyc"),
                    entity("HighRisk", "AccountTier", "HighRisk accounts must complete KycPolicy verification before the first payout.", "kb-kyc"),
                ],
                facts: vec![
                    fact("KycPolicy requires government ID and proof of address within 30 days.", &["KycPolicy"], "kb-kyc"),
                    fact("HighRisk accounts must complete KycPolicy verification before the first payout.", &["HighRisk"], "kb-kyc"),
                ],
                relations: vec![rel("HighRisk", "requires", "KycPolicy", "kb-kyc")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-fraud",
            chunks: &[
                "FraudEngine screens every payout above 10000 dollars.",
                "FraudEngine depends on RiskRules.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("FraudEngine", "Service", "FraudEngine screens every payout above 10000 dollars.", "kb-fraud"),
                    entity("RiskRules", "Ruleset", "FraudEngine depends on RiskRules.", "kb-fraud"),
                ],
                facts: vec![fact("FraudEngine screens every payout above 10000 dollars.", &["FraudEngine"], "kb-fraud")],
                relations: vec![rel("FraudEngine", "depends_on", "RiskRules", "kb-fraud")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-risk-rules",
            chunks: &[
                "RiskRules block accounts with more than 50 payouts per day.",
                "RiskRules updates the country blocklist every quarter.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("RiskRules", "Ruleset", "RiskRules block accounts with more than 50 payouts per day.", "kb-risk-rules")],
                facts: vec![
                    fact("RiskRules block accounts with more than 50 payouts per day.", &["RiskRules"], "kb-risk-rules"),
                    fact("RiskRules updates the country blocklist every quarter.", &["RiskRules"], "kb-risk-rules"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-risk-change",
            chunks: &[
                "RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.",
                "RiskRules screened payouts above 20000 dollars before February.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("RiskRules", "Ruleset", "RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.", "kb-risk-change")],
                facts: vec![
                    fact("RiskRules lowered the payout threshold from 20000 to 10000 dollars in February.", &["RiskRules"], "kb-risk-change"),
                    fact("RiskRules screened payouts above 20000 dollars before February.", &["RiskRules"], "kb-risk-change"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-billing",
            chunks: &[
                "The BillingCycle issues invoices on the first day of each month.",
                "Overdue invoices enter the DunningFlow on day 15.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("BillingCycle", "Process", "The BillingCycle issues invoices on the first day of each month.", "kb-billing"),
                    entity("DunningFlow", "Process", "Overdue invoices enter the DunningFlow on day 15.", "kb-billing"),
                ],
                facts: vec![
                    fact("The BillingCycle issues invoices on the first day of each month.", &["BillingCycle"], "kb-billing"),
                    fact("Overdue invoices enter the DunningFlow on day 15.", &["DunningFlow"], "kb-billing"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-refund",
            chunks: &[
                "RefundPolicy grants a full refund within 14 days.",
                "RefundPolicy grants a 50 percent refund between 15 and 30 days.",
                "RefundPolicy grants no refund after 30 days.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("RefundPolicy", "Policy", "RefundPolicy grants a full refund within 14 days.", "kb-refund")],
                facts: vec![
                    fact("RefundPolicy grants a full refund within 14 days.", &["RefundPolicy"], "kb-refund"),
                    fact("RefundPolicy grants a 50 percent refund between 15 and 30 days.", &["RefundPolicy"], "kb-refund"),
                    fact("RefundPolicy grants no refund after 30 days.", &["RefundPolicy"], "kb-refund"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-refund-conflict",
            chunks: &[
                "The SupportFaQ says refunds apply within 30 days.",
                "The SupportFaQ conflicts with RefundPolicy.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("SupportFaQ", "Page", "The SupportFaQ says refunds apply within 30 days.", "kb-refund-conflict"),
                    entity("RefundPolicy", "Policy", "The SupportFaQ conflicts with RefundPolicy.", "kb-refund-conflict"),
                ],
                facts: vec![
                    fact("The SupportFaQ says refunds apply within 30 days.", &["SupportFaQ"], "kb-refund-conflict"),
                    fact("The SupportFaQ conflicts with RefundPolicy.", &["SupportFaQ"], "kb-refund-conflict"),
                ],
                relations: vec![rel("SupportFaQ", "conflicts_with", "RefundPolicy", "kb-refund-conflict")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-dunning",
            chunks: &[
                "The DunningFlow sends a reminder on day 15.",
                "The DunningFlow sends a warning on day 22 and suspends the account on day 30.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("DunningFlow", "Process", "The DunningFlow sends a reminder on day 15.", "kb-dunning")],
                facts: vec![
                    fact("The DunningFlow sends a reminder on day 15.", &["DunningFlow"], "kb-dunning"),
                    fact("The DunningFlow sends a warning on day 22 and suspends the account on day 30.", &["DunningFlow"], "kb-dunning"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-retention-v1",
            chunks: &["RetentionV1 keeps user records for 12 months."],
            ir: KnowledgeIr {
                entities: vec![entity("RetentionV1", "PolicyVersion", "RetentionV1 keeps user records for 12 months.", "kb-retention-v1")],
                facts: vec![fact("RetentionV1 keeps user records for 12 months.", &["RetentionV1"], "kb-retention-v1")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-retention-v2",
            chunks: &["RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months."],
            ir: KnowledgeIr {
                entities: vec![
                    entity("RetentionV2", "PolicyVersion", "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.", "kb-retention-v2"),
                    entity("RetentionV1", "PolicyVersion", "RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.", "kb-retention-v2"),
                ],
                facts: vec![fact("RetentionV2 replaced RetentionV1 in May and keeps user records for 18 months.", &["RetentionV2"], "kb-retention-v2")],
                relations: vec![rel("RetentionV2", "replaced", "RetentionV1", "kb-retention-v2")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-dsar",
            chunks: &[
                "DsarPolicy requires answering subject access requests within 30 days.",
                "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DsarPolicy", "Policy", "DsarPolicy requires answering subject access requests within 30 days.", "kb-dsar"),
                    entity("DataCatalog", "Registry", "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.", "kb-dsar"),
                    entity("UsersDB", "Store", "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.", "kb-dsar"),
                    entity("LogWarehouse", "Store", "The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.", "kb-dsar"),
                ],
                facts: vec![
                    fact("DsarPolicy requires answering subject access requests within 30 days.", &["DsarPolicy"], "kb-dsar"),
                    fact("The DataCatalog maps PII to UsersDB and raw logs to LogWarehouse.", &["DataCatalog"], "kb-dsar"),
                ],
                relations: vec![
                    rel("DataCatalog", "maps", "UsersDB", "kb-dsar"),
                    rel("DataCatalog", "maps", "LogWarehouse", "kb-dsar"),
                ],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-users-db",
            chunks: &[
                "UsersDB encrypts PII at rest.",
                "UsersDB runs nightly integrity checks.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("UsersDB", "Store", "UsersDB encrypts PII at rest.", "kb-users-db")],
                facts: vec![
                    fact("UsersDB encrypts PII at rest.", &["UsersDB"], "kb-users-db"),
                    fact("UsersDB runs nightly integrity checks.", &["UsersDB"], "kb-users-db"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-vendor-cdn",
            chunks: &[
                "CdnVendor provides the content delivery network under a 99.9 percent SLA.",
                "CdnVendor sources capacity from EdgeNodes.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CdnVendor", "Vendor", "CdnVendor provides the content delivery network under a 99.9 percent SLA.", "kb-vendor-cdn"),
                    entity("EdgeNodes", "Infrastructure", "CdnVendor sources capacity from EdgeNodes.", "kb-vendor-cdn"),
                ],
                facts: vec![
                    fact("CdnVendor provides the content delivery network under a 99.9 percent SLA.", &["CdnVendor"], "kb-vendor-cdn"),
                    fact("CdnVendor sources capacity from EdgeNodes.", &["CdnVendor"], "kb-vendor-cdn"),
                ],
                relations: vec![rel("CdnVendor", "uses", "EdgeNodes", "kb-vendor-cdn")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-vendor-payment",
            chunks: &[
                "ProcessorVendor is PCI-DSS certified.",
                "ProcessorVendor processes all card payments.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("ProcessorVendor", "Vendor", "ProcessorVendor is PCI-DSS certified.", "kb-vendor-payment")],
                facts: vec![
                    fact("ProcessorVendor is PCI-DSS certified.", &["ProcessorVendor"], "kb-vendor-payment"),
                    fact("ProcessorVendor processes all card payments.", &["ProcessorVendor"], "kb-vendor-payment"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-vendor-email",
            chunks: &["MailVendor handles transactional email with a 99 percent SLA."],
            ir: KnowledgeIr {
                entities: vec![entity("MailVendor", "Vendor", "MailVendor handles transactional email with a 99 percent SLA.", "kb-vendor-email")],
                facts: vec![fact("MailVendor handles transactional email with a 99 percent SLA.", &["MailVendor"], "kb-vendor-email")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-vendor-sla",
            chunks: &[
                "VendorSlaPolicy requires quarterly SLA reviews for every vendor.",
                "CdnVendor failed its SLA review in March.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("VendorSlaPolicy", "Policy", "VendorSlaPolicy requires quarterly SLA reviews for every vendor.", "kb-vendor-sla"),
                    entity("CdnVendor", "Vendor", "CdnVendor failed its SLA review in March.", "kb-vendor-sla"),
                ],
                facts: vec![
                    fact("VendorSlaPolicy requires quarterly SLA reviews for every vendor.", &["VendorSlaPolicy"], "kb-vendor-sla"),
                    fact("CdnVendor failed its SLA review in March.", &["CdnVendor"], "kb-vendor-sla"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-procurement",
            chunks: &[
                "ProcurementRule requires two vendor bids and a SecurityReview.",
                "The SecurityReview checks vendor access to card data.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("ProcurementRule", "Policy", "ProcurementRule requires two vendor bids and a SecurityReview.", "kb-procurement"),
                    entity("SecurityReview", "Process", "The SecurityReview checks vendor access to card data.", "kb-procurement"),
                ],
                facts: vec![
                    fact("ProcurementRule requires two vendor bids and a SecurityReview.", &["ProcurementRule"], "kb-procurement"),
                    fact("The SecurityReview checks vendor access to card data.", &["SecurityReview"], "kb-procurement"),
                ],
                relations: vec![rel("ProcurementRule", "requires", "SecurityReview", "kb-procurement")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-runbook-deploy",
            chunks: &[
                "The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.",
                "The DeployRunbook rolls back the canary if the error rate exceeds 1 percent.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("DeployRunbook", "Runbook", "The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.", "kb-runbook-deploy")],
                facts: vec![
                    fact("The DeployRunbook starts with a canary deploy, then observes metrics for 15 minutes, then rolls out the full fleet.", &["DeployRunbook"], "kb-runbook-deploy"),
                    fact("The DeployRunbook rolls back the canary if the error rate exceeds 1 percent.", &["DeployRunbook"], "kb-runbook-deploy"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-runbook-db",
            chunks: &[
                "The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.",
                "The DbFailoverRunbook files an IncidentRecord after every failover.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DbFailoverRunbook", "Runbook", "The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.", "kb-runbook-db"),
                    entity("IncidentRecord", "Record", "The DbFailoverRunbook files an IncidentRecord after every failover.", "kb-runbook-db"),
                ],
                facts: vec![
                    fact("The DbFailoverRunbook promotes the standby replica, then verifies replication lag, then repoints the application.", &["DbFailoverRunbook"], "kb-runbook-db"),
                    fact("The DbFailoverRunbook files an IncidentRecord after every failover.", &["DbFailoverRunbook"], "kb-runbook-db"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-runbook-sev1",
            chunks: &[
                "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.",
                "The Sev1Runbook requires a postmortem within 5 business days.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("Sev1Runbook", "Runbook", "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.", "kb-runbook-sev1"),
                    entity("DutyManager", "Team", "The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.", "kb-runbook-sev1"),
                ],
                facts: vec![
                    fact("The Sev1Runbook pages the DutyManager, opens a war room, updates the status page, and files a postmortem.", &["Sev1Runbook"], "kb-runbook-sev1"),
                    fact("The Sev1Runbook requires a postmortem within 5 business days.", &["Sev1Runbook"], "kb-runbook-sev1"),
                ],
                relations: vec![rel("Sev1Runbook", "pages", "DutyManager", "kb-runbook-sev1")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-onboard-flow",
            chunks: &["The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account."],
            ir: KnowledgeIr {
                entities: vec![
                    entity("OnboardingFlow", "Process", "The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account.", "kb-onboard-flow"),
                    entity("KycPolicy", "Policy", "The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account.", "kb-onboard-flow"),
                ],
                facts: vec![fact("The OnboardingFlow verifies the email, then runs the KycPolicy checks, then funds the wallet, then activates the account.", &["OnboardingFlow"], "kb-onboard-flow")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-roadmap",
            chunks: &[
                "The Q3Roadmap planned the RefundAutomation feature.",
                "RefundAutomation shipped in July.",
                "The Q4Roadmap plans the MultiCurrency feature.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("Q3Roadmap", "Plan", "The Q3Roadmap planned the RefundAutomation feature.", "kb-roadmap"),
                    entity("RefundAutomation", "Feature", "The Q3Roadmap planned the RefundAutomation feature.", "kb-roadmap"),
                    entity("Q4Roadmap", "Plan", "The Q4Roadmap plans the MultiCurrency feature.", "kb-roadmap"),
                    entity("MultiCurrency", "Feature", "The Q4Roadmap plans the MultiCurrency feature.", "kb-roadmap"),
                ],
                facts: vec![
                    fact("The Q3Roadmap planned the RefundAutomation feature.", &["Q3Roadmap"], "kb-roadmap"),
                    fact("RefundAutomation shipped in July.", &["RefundAutomation"], "kb-roadmap"),
                    fact("The Q4Roadmap plans the MultiCurrency feature.", &["Q4Roadmap"], "kb-roadmap"),
                ],
                relations: vec![
                    rel("Q3Roadmap", "planned", "RefundAutomation", "kb-roadmap"),
                    rel("Q4Roadmap", "plans", "MultiCurrency", "kb-roadmap"),
                ],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-changelog",
            chunks: &[
                "CheckoutService gained the OneClick feature in April.",
                "The OneClick feature increased checkout conversion by 12 percent.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CheckoutService", "Service", "CheckoutService gained the OneClick feature in April.", "kb-changelog"),
                    entity("OneClick", "Feature", "CheckoutService gained the OneClick feature in April.", "kb-changelog"),
                ],
                facts: vec![
                    fact("CheckoutService gained the OneClick feature in April.", &["CheckoutService"], "kb-changelog"),
                    fact("The OneClick feature increased checkout conversion by 12 percent.", &["OneClick"], "kb-changelog"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-deploy-window",
            chunks: &[
                "The EngineeringMemo sets the deploy window at midnight.",
                "The SreRunbook sets the deploy window at 6 am.",
                "The EngineeringMemo conflicts with the SreRunbook.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("EngineeringMemo", "Memo", "The EngineeringMemo sets the deploy window at midnight.", "kb-deploy-window"),
                    entity("SreRunbook", "Runbook", "The SreRunbook sets the deploy window at 6 am.", "kb-deploy-window"),
                ],
                facts: vec![
                    fact("The EngineeringMemo sets the deploy window at midnight.", &["EngineeringMemo"], "kb-deploy-window"),
                    fact("The SreRunbook sets the deploy window at 6 am.", &["SreRunbook"], "kb-deploy-window"),
                    fact("The EngineeringMemo conflicts with the SreRunbook.", &["EngineeringMemo"], "kb-deploy-window"),
                ],
                relations: vec![rel("EngineeringMemo", "conflicts_with", "SreRunbook", "kb-deploy-window")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-db-choice",
            chunks: &[
                "The DbArchitectureDecision mandates Postgres as the primary database.",
                "The PilotProposal proposes MySQL for the analytics workload.",
                "The DbArchitectureDecision conflicts with the PilotProposal.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("DbArchitectureDecision", "Decision", "The DbArchitectureDecision mandates Postgres as the primary database.", "kb-db-choice"),
                    entity("PilotProposal", "Proposal", "The PilotProposal proposes MySQL for the analytics workload.", "kb-db-choice"),
                ],
                facts: vec![
                    fact("The DbArchitectureDecision mandates Postgres as the primary database.", &["DbArchitectureDecision"], "kb-db-choice"),
                    fact("The PilotProposal proposes MySQL for the analytics workload.", &["PilotProposal"], "kb-db-choice"),
                    fact("The DbArchitectureDecision conflicts with the PilotProposal.", &["DbArchitectureDecision"], "kb-db-choice"),
                ],
                relations: vec![rel("DbArchitectureDecision", "conflicts_with", "PilotProposal", "kb-db-choice")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-support-hours",
            chunks: &[
                "The SupportPage lists support hours as 9 to 5 weekdays.",
                "The InternalWiki lists support hours as 8 to 6 weekdays.",
                "The SupportPage conflicts with the InternalWiki on support hours.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("SupportPage", "Page", "The SupportPage lists support hours as 9 to 5 weekdays.", "kb-support-hours"),
                    entity("InternalWiki", "Wiki", "The InternalWiki lists support hours as 8 to 6 weekdays.", "kb-support-hours"),
                ],
                facts: vec![
                    fact("The SupportPage lists support hours as 9 to 5 weekdays.", &["SupportPage"], "kb-support-hours"),
                    fact("The InternalWiki lists support hours as 8 to 6 weekdays.", &["InternalWiki"], "kb-support-hours"),
                    fact("The SupportPage conflicts with the InternalWiki on support hours.", &["SupportPage"], "kb-support-hours"),
                ],
                relations: vec![rel("SupportPage", "conflicts_with", "InternalWiki", "kb-support-hours")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-oncall-conflict",
            chunks: &[
                "The SchedulingMemo says Alex runs Mondays.",
                "The TeamWiki says Alex runs Tuesdays.",
                "The SchedulingMemo conflicts with the TeamWiki.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("SchedulingMemo", "Memo", "The SchedulingMemo says Alex runs Mondays.", "kb-oncall-conflict"),
                    entity("TeamWiki", "Wiki", "The TeamWiki says Alex runs Tuesdays.", "kb-oncall-conflict"),
                    entity("Alex", "Engineer", "The SchedulingMemo says Alex runs Mondays.", "kb-oncall-conflict"),
                ],
                facts: vec![
                    fact("The SchedulingMemo says Alex runs Mondays.", &["SchedulingMemo"], "kb-oncall-conflict"),
                    fact("The TeamWiki says Alex runs Tuesdays.", &["TeamWiki"], "kb-oncall-conflict"),
                    fact("The SchedulingMemo conflicts with the TeamWiki.", &["SchedulingMemo"], "kb-oncall-conflict"),
                ],
                relations: vec![rel("SchedulingMemo", "conflicts_with", "TeamWiki", "kb-oncall-conflict")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-cust-alex",
            chunks: &[
                "CustomerAlex subscribed to the ProPlan with 5 seats.",
                "CustomerAlex set the support language preference to Spanish.",
                "CustomerAlex renewed the ProPlan in May.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CustomerAlex", "Customer", "CustomerAlex subscribed to the ProPlan with 5 seats.", "kb-cust-alex"),
                    entity("ProPlan", "Plan", "CustomerAlex subscribed to the ProPlan with 5 seats.", "kb-cust-alex"),
                ],
                facts: vec![
                    fact("CustomerAlex subscribed to the ProPlan with 5 seats.", &["CustomerAlex"], "kb-cust-alex"),
                    fact("CustomerAlex set the support language preference to Spanish.", &["CustomerAlex"], "kb-cust-alex"),
                    fact("CustomerAlex renewed the ProPlan in May.", &["CustomerAlex"], "kb-cust-alex"),
                ],
                relations: vec![rel("CustomerAlex", "subscribed_to", "ProPlan", "kb-cust-alex")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-cust-priya",
            chunks: &[
                "CustomerPriya subscribed to the BasicPlan with 2 seats.",
                "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.",
                "CustomerPriya set the support language preference to French.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CustomerPriya", "Customer", "CustomerPriya subscribed to the BasicPlan with 2 seats.", "kb-cust-priya"),
                    entity("BasicPlan", "Plan", "CustomerPriya subscribed to the BasicPlan with 2 seats.", "kb-cust-priya"),
                    entity("ProPlan", "Plan", "CustomerPriya upgraded from the BasicPlan to the ProPlan in September.", "kb-cust-priya"),
                ],
                facts: vec![
                    fact("CustomerPriya subscribed to the BasicPlan with 2 seats.", &["CustomerPriya"], "kb-cust-priya"),
                    fact("CustomerPriya upgraded from the BasicPlan to the ProPlan in September.", &["CustomerPriya"], "kb-cust-priya"),
                    fact("CustomerPriya set the support language preference to French.", &["CustomerPriya"], "kb-cust-priya"),
                ],
                relations: vec![rel("CustomerPriya", "upgraded_from", "BasicPlan", "kb-cust-priya")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-cust-dev",
            chunks: &[
                "CustomerDev subscribed to the ProPlan with 12 seats.",
                "CustomerDev prefers weekly summary emails.",
                "CustomerDev renewed the ProPlan in June.",
            ],
            ir: KnowledgeIr {
                entities: vec![
                    entity("CustomerDev", "Customer", "CustomerDev subscribed to the ProPlan with 12 seats.", "kb-cust-dev"),
                    entity("ProPlan", "Plan", "CustomerDev subscribed to the ProPlan with 12 seats.", "kb-cust-dev"),
                ],
                facts: vec![
                    fact("CustomerDev subscribed to the ProPlan with 12 seats.", &["CustomerDev"], "kb-cust-dev"),
                    fact("CustomerDev prefers weekly summary emails.", &["CustomerDev"], "kb-cust-dev"),
                    fact("CustomerDev renewed the ProPlan in June.", &["CustomerDev"], "kb-cust-dev"),
                ],
                relations: vec![rel("CustomerDev", "subscribed_to", "ProPlan", "kb-cust-dev")],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-dr-plan",
            chunks: &["The DrPlan document is still under review."],
            ir: KnowledgeIr {
                entities: vec![entity("DrPlan", "Document", "The DrPlan document is still under review.", "kb-dr-plan")],
                facts: vec![fact("The DrPlan document is still under review.", &["DrPlan"], "kb-dr-plan")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-api-limits",
            chunks: &[
                "The PublicApi allows 100 requests per minute per key.",
                "The PublicApi returns a 429 status when the limit is exceeded.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("PublicApi", "Service", "The PublicApi allows 100 requests per minute per key.", "kb-api-limits")],
                facts: vec![
                    fact("The PublicApi allows 100 requests per minute per key.", &["PublicApi"], "kb-api-limits"),
                    fact("The PublicApi returns a 429 status when the limit is exceeded.", &["PublicApi"], "kb-api-limits"),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// W31-DEC-001 / W31-TEMP-001 scenario corpora (decision + timeline).
//
// These are NOT part of the frozen 148-task union corpus — they back the
// evidence-to-decision and historical-vs-current experiments, whose
// supersession lineage lives in the kernel (wave31_decision.rs) rather
// than in the docs. Design rules:
// - every stale statement is an exact chunk sentence of its version doc,
//   so the exact-substring stale check in wave31_decision.rs works;
// - the current doc carries the full change history as past-tense facts
//   ("was … until March"), so history is answerable without AS_OF while
//   the superseded statements stay distinguishable from the histories.
// ---------------------------------------------------------------------------

/// v1 superseded claim (DEC-001).
pub const DEC_V1: &str = "DeployPolicy sets the deployment window to Friday evening.";
/// v2 superseded claim (DEC-001).
pub const DEC_V2: &str = "DeployPolicy sets the deployment window to Wednesday 10:00-12:00 UTC.";
/// v3 current claim (DEC-001).
pub const DEC_V3: &str = "DeployPolicy sets the deployment window to Tuesday 02:00-04:00 UTC.";
/// The live conflicting claim (DEC-001): current, never superseded.
pub const DEC_RUNBOOK: &str = "DeployRunbook allows deployment on any weekday evening.";

/// v1 superseded claim (TEMP-001).
pub const RET_V1: &str = "RetryLimit is 2 attempts.";
/// v2 superseded claim (TEMP-001).
pub const RET_V2: &str = "RetryLimit is 3 attempts.";
/// v3 current claim (TEMP-001).
pub const RET_V3: &str = "RetryLimit is 5 attempts.";

/// DEC-001 decision scenario: policy lineage v1→v2→v3 (superseded in the
/// kernel) plus a live conflicting runbook.
pub fn decision_docs() -> Vec<Doc> {
    vec![
        Doc {
            id: "kb-deploy-v1",
            chunks: &[DEC_V1],
            ir: KnowledgeIr {
                entities: vec![entity("DeployPolicy", "Policy", DEC_V1, "kb-deploy-v1")],
                facts: vec![fact(DEC_V1, &["DeployPolicy"], "kb-deploy-v1")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-deploy-v2",
            chunks: &[DEC_V2],
            ir: KnowledgeIr {
                entities: vec![entity("DeployPolicy", "Policy", DEC_V2, "kb-deploy-v2")],
                facts: vec![fact(DEC_V2, &["DeployPolicy"], "kb-deploy-v2")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-deploy-policy",
            chunks: &[
                DEC_V3,
                "The deployment window was Friday evening until March.",
                "The deployment window was Wednesday 10:00-12:00 UTC from March to June.",
                "Deploying on Friday evening violates the current policy.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("DeployPolicy", "Policy", DEC_V3, "kb-deploy-policy")],
                facts: vec![
                    fact(DEC_V3, &["DeployPolicy"], "kb-deploy-policy"),
                    fact(
                        "The deployment window was Friday evening until March.",
                        &["DeployPolicy"],
                        "kb-deploy-policy",
                    ),
                    fact(
                        "The deployment window was Wednesday 10:00-12:00 UTC from March to June.",
                        &["DeployPolicy"],
                        "kb-deploy-policy",
                    ),
                    fact(
                        "Deploying on Friday evening violates the current policy.",
                        &["DeployPolicy"],
                        "kb-deploy-policy",
                    ),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-deploy-runbook",
            chunks: &[DEC_RUNBOOK],
            ir: KnowledgeIr {
                entities: vec![entity(
                    "DeployRunbook",
                    "Documentation",
                    DEC_RUNBOOK,
                    "kb-deploy-runbook",
                )],
                facts: vec![fact(DEC_RUNBOOK, &["DeployRunbook"], "kb-deploy-runbook")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
    ]
}

/// TEMP-001 timeline: retry-limit lineage v1→v2→v3, with the current doc
/// stating the full history and the reasons (change/why dimensions).
pub fn timeline_docs() -> Vec<Doc> {
    vec![
        Doc {
            id: "kb-retry-v1",
            chunks: &[RET_V1],
            ir: KnowledgeIr {
                entities: vec![entity("RetryLimit", "Setting", RET_V1, "kb-retry-v1")],
                facts: vec![fact(RET_V1, &["RetryLimit"], "kb-retry-v1")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-retry-v2",
            chunks: &[RET_V2],
            ir: KnowledgeIr {
                entities: vec![entity("RetryLimit", "Setting", RET_V2, "kb-retry-v2")],
                facts: vec![fact(RET_V2, &["RetryLimit"], "kb-retry-v2")],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
        Doc {
            id: "kb-retry-v3",
            chunks: &[
                RET_V3,
                "The retry limit was 2 attempts in January and February.",
                "The retry limit was 3 attempts from March to June.",
                "The limit rose in March due to queue backlog, and in June for DDoS defense.",
            ],
            ir: KnowledgeIr {
                entities: vec![entity("RetryLimit", "Setting", RET_V3, "kb-retry-v3")],
                facts: vec![
                    fact(RET_V3, &["RetryLimit"], "kb-retry-v3"),
                    fact(
                        "The retry limit was 2 attempts in January and February.",
                        &["RetryLimit"],
                        "kb-retry-v3",
                    ),
                    fact(
                        "The retry limit was 3 attempts from March to June.",
                        &["RetryLimit"],
                        "kb-retry-v3",
                    ),
                    fact(
                        "The limit rose in March due to queue backlog, and in June for DDoS defense.",
                        &["RetryLimit"],
                        "kb-retry-v3",
                    ),
                ],
                relations: vec![],
                ..KnowledgeIr::default()
            },
        },
    ]
}
