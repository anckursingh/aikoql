//! Extracted verbatim from server.rs (PRR-7). No behavior changes.

use crate::*;

use crate::audit::*;
use crate::authz::*;
use crate::session::*;
use crate::tools::*;

use crate::protocol::*;

pub(crate) fn tools_list() -> J {
    let subj = json!({"type": "string", "description": "calling principal (default: mcp-agent)"});
    let koid = json!({"type": "string", "description": "32-char hex KOID"});
    json!({
        "tools": [
            {"name": "remember", "description": "Commit a knowledge object (or new version) with provenance. Set embed:true for auto-embedding via SemanticEngine (MRFC-0040). Returns KOID+version.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "koid": koid, "properties": {"type": "object"}, "semantic": {"type": "object"}, "embed": {"type": "boolean", "description": "Request auto-embedding via configured AI provider (MRFC-0040)"}, "expected_version": {"type": "integer"}, "idempotency_key": {"type": "string"}, "note": {"type": "string"}}, "required": ["type_name"]}},
            {"name": "forget", "description": "Tombstone or legally erase a knowledge object (audit-preserving).", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "mode": {"type": "string", "enum": ["tombstone", "erase"]}}, "required": ["koid"]}},
            {"name": "evolve", "description": "Transition a knowledge object along its lifecycle (draft->active->verified->archived->deleted).", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "to": {"type": "string"}}, "required": ["koid", "to"]}},
            {"name": "transition_epistemic", "description": "Move a knowledge object's epistemic status under the constrained transition table (how we know it is true: observed/extracted/asserted/inferred/verified/contradicted/superseded). Superseding a fact ends its validity now and, when superseded_by names the successor, wires the SUPERSEDES edge.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "to": {"type": "string"}, "reason": {"type": "string"}, "expected_version": {"type": "integer"}, "superseded_by": {"type": "string", "description": "KOID of the successor fact (transition to 'superseded' only)"}}, "required": ["koid", "to"]}},
            {"name": "derive", "description": "Derive a knowledge object from premise KOs (first-class lineage). Validates every source, wires DERIVED_FROM edges, and stamps a derivation record answering WHY (reason) / FROM WHAT (sources) / DERIVED HOW (operation, model) / BY WHOM (actor) / WHEN (timestamp) / WITH WHICH EVIDENCE.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "properties": {"type": "object"}, "sources": {"type": "array", "items": {"type": "string"}, "description": "Premise KOIDs (hex)"}, "operation": {"type": "string", "description": "Derivation operation (rule_fired, inference, merge, extraction…)"}, "actor": {"type": "string", "description": "Who performed the derivation (default: subject)"}, "model": {"type": "string", "description": "Model used, if model-assisted"}, "reason": {"type": "string", "description": "Human-readable justification (the WHY)"}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "confidence": {"type": "object", "description": "Confidence context override: {score, confirmations, last_verified?}; default derives a baseline from the sources"}}, "required": ["type_name"]}},
            {"name": "observe", "description": "Record a direct observation of the world. Evidence is mandatory — an unbacked observation is rejected, not downgraded. Stamps epistemic Observed + canonical evidence.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "properties": {"type": "object"}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "valid_from": {"type": "integer"}, "note": {"type": "string"}}, "required": ["type_name", "evidence"]}},
            {"name": "assert_knowledge", "description": "Assert knowledge on explicit authority. Evidence AND a valid authority level are mandatory (human_approved, organization_policy, architecture_decision, source_code, test_verified, ci_verified, deployment_observed, documentation, agent_derived, llm_inferred, untrusted_external).", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "properties": {"type": "object"}, "authority": {"type": "string"}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "valid_from": {"type": "integer"}, "note": {"type": "string"}}, "required": ["type_name", "authority", "evidence"]}},
            {"name": "verify_knowledge", "description": "Independently verify a KO. Not a status flip: evidence is mandatory, the confidence context is bumped (confirmations + last_verified, score never lowered), and the epistemic transition is recorded.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "confidence": {"type": "number", "description": "Optional verification confidence; never lowers the existing score"}, "note": {"type": "string"}}, "required": ["koid", "evidence"]}},
            {"name": "contradict", "description": "Register a competing assertion against an existing claim. Creates the counter-claim (CONTRADICTS edge) and a persisted Conflict KO with per-assertion authority/evidence/timestamp/scope snapshots. The original claim's status is untouched until a resolution decision.", "inputSchema": {"type": "object", "properties": {"subject": subj, "claim": koid, "counter_type": {"type": "string", "description": "Type of the counter-claim KO (default: Claim)"}, "properties": {"type": "object", "description": "Counter-claim properties"}, "authority": {"type": "string", "description": "Authority level of the counter-assertion"}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "note": {"type": "string"}}, "required": ["claim", "evidence"]}},
            {"name": "supersede", "description": "Replace a claim with a new generation. Preserves the old KO (Superseded + valid_to + SUPERSEDES edge) and sweeps its derived dependents for staleness (invalidation stamp).", "inputSchema": {"type": "object", "properties": {"subject": subj, "old": koid, "type_name": {"type": "string"}, "properties": {"type": "object"}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "reason": {"type": "string"}, "note": {"type": "string"}}, "required": ["old", "type_name", "evidence"]}},
            {"name": "merge", "description": "Merge 2+ sources into one KO as a first-class derivation (operation 'merge', DERIVED_FROM every source). Strategy: manual (caller properties) | newest_wins | authority_wins.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "sources": {"type": "array", "items": {"type": "string"}, "description": "Source KOIDs (hex), at least two"}, "properties": {"type": "object", "description": "Merged result (required for strategy manual)"}, "strategy": {"type": "string", "enum": ["manual", "newest_wins", "authority_wins"]}, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "reason": {"type": "string"}, "note": {"type": "string"}}, "required": ["type_name", "sources"]}},
            {"name": "invalidate", "description": "Withdraw support for a KO and everything derived from it. Evidence is mandatory. The target transitions to Contradicted where legal; every DERIVED_FROM dependent gets the invalidation stamp + valid_to=now but keeps its epistemic status.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "evidence": {"type": "array", "items": {"type": "object"}, "description": "Canonical evidence entries: {source_artifact, method, location?, revision?, confidence?}"}, "reason": {"type": "string"}, "note": {"type": "string"}}, "required": ["koid", "evidence"]}},
            {"name": "resolve_conflict", "description": "Apply a resolution decision to a persisted Conflict KO: resolved_a_preferred | resolved_b_preferred | resolved_both_valid | resolved_replaced. Rationale is mandatory — the kernel never silently picks a side.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "decision": {"type": "string", "enum": ["resolved_a_preferred", "resolved_b_preferred", "resolved_both_valid", "resolved_replaced"]}, "rationale": {"type": "string"}, "replacement": {"type": "string", "description": "Replacement claim KOID (required for resolved_replaced)"}}, "required": ["koid", "decision", "rationale"]}},
            {"name": "resolve_conflict_by_authority", "description": "Resolve a Conflict KO by the recorded authority of each assertion — higher authority wins; a tie is an error, never a silent pick.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "rationale": {"type": "string"}}, "required": ["koid", "rationale"]}},
            {"name": "verify", "description": "Check whether a subject may perform an action on an object.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "action": {"type": "string"}}, "required": ["koid", "action"]}},
            {"name": "get", "description": "Fetch a knowledge object by KOID.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "find_similar", "description": "Hybrid recall: vector + text + filters with RRF/weighted fusion.", "inputSchema": {"type": "object", "properties": {"subject": subj, "text": {"type": "string"}, "vector": {"type": "array"}, "embedding_model": {"type": "string", "description": "When set, only vectors from this embedding model are considered"}, "k": {"type": "integer"}, "fusion": {"type": "string"}, "type_name": {"type": "string"}}}},
            {"name": "trace", "description": "Full lineage of a fact: versions + events.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "explain", "description": "Why is this believed: provenance, source, confidence, evidence.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "version": {"type": "integer"}}, "required": ["koid"]}},
            {"name": "prove", "description": "Verify the hash-chained audit trail for a claim.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "provenance", "description": "Render the full provenance chain as markdown: source, evidence, audit trail.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid}, "required": ["koid"]}},
            {"name": "relate", "description": "Add a directed relationship edge from one KO to another.", "inputSchema": {"type": "object", "properties": {"subject": subj, "from": koid, "to": koid, "rel_type": {"type": "string"}}, "required": ["from", "to", "rel_type"]}},
            {"name": "traverse", "description": "Walk relationship edges from a starting KO up to a depth.", "inputSchema": {"type": "object", "properties": {"subject": subj, "koid": koid, "rel_type": {"type": "string"}, "depth": {"type": "integer"}}, "required": ["koid"]}},
            {"name": "eval_recall", "description": "Measure recall@k against an expected KOID set.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "text": {"type": "string"}, "vector": {"type": "array"}, "k": {"type": "integer"}, "fusion": {"type": "string"}, "expected": {"type": "array", "items": {"type": "string"}}}, "required": ["expected"]}},
            {"name": "eval_staleness", "description": "Report index_lag_ms distribution for a recall query.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "text": {"type": "string"}, "vector": {"type": "array"}, "k": {"type": "integer"}, "fusion": {"type": "string"}}}},
            {"name": "eval_contradictions", "description": "Find same-type, high-similarity object pairs whose property values differ.", "inputSchema": {"type": "object", "properties": {"subject": subj, "type_name": {"type": "string"}, "property": {"type": "string"}, "threshold": {"type": "number"}, "max_results": {"type": "integer"}}, "required": ["type_name", "property"]}},
            {"name": "aikoql", "description": "Execute an aikoql query (text-based knowledge query language). Supports MATCH, WHERE, SIMILAR TO, TRAVERSE, AS_OF, BETWEEN, HISTORICAL, EPISTEMIC, RETURN, CREATE, UPDATE, DELETE.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string", "description": "aikoql query text"}, "subject": {"type": "string", "description": "Calling principal for ACL (default: query-user)"}}, "required": ["query"]}},
            {"name": "backup", "description": "Create a timestamped backup of the database.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "restore", "description": "Restore the database from a backup directory.", "inputSchema": {"type": "object", "properties": {"backup": {"type": "string", "description": "Backup directory name"}}, "required": ["backup"]}},
            {"name": "list_backups", "description": "List available backups in the current directory.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "verify_backup", "description": "Verify a backup by opening it in a temporary kernel and checking journal + object count integrity.", "inputSchema": {"type": "object", "properties": {"backup": {"type": "string", "description": "Backup directory name"}}, "required": ["backup"]}},
            {"name": "metrics", "description": "Return database metrics: journal sequence, object counts, uptime.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "audit_report", "description": "Generate a compliance audit report with full object inventory and audit chain hash.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "compliance_report", "description": "Generate an encryption compliance report: policies, key inventory, audit events, compliance grade (A/C).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "reason", "description": "Execute a reasoning rule: find objects matching properties and produce provenance-tagged claims.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "properties": {"type": "object"}}, "required": ["type_name"]}},
            {"name": "infer", "description": "Infer similar knowledge: find objects textually similar to a query within a type.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "text": {"type": "string"}}, "required": ["type_name"]}},
            {"name": "predict", "description": "Predict properties for a target object based on top-k similar objects.", "inputSchema": {"type": "object", "properties": {"type_name": {"type": "string"}, "properties": {"type": "object"}, "k": {"type": "integer"}}, "required": ["type_name"]}},
            {"name": "abi_version", "description": "Return ABI version and exportable audit chain for offline verification.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_program", "description": "Deploy an aikoql program as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "body": {"type": "string"}, "language": {"type": "string"}}, "required": ["name", "body"]}},
            {"name": "execute_program", "description": "Execute a deployed program KO by KOID with optional parameters.", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "params": {"type": "object"}}, "required": ["koid"]}},
            {"name": "list_programs", "description": "List all deployed program Knowledge Objects.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_policy", "description": "Deploy an RBAC policy as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "effect": {"type": "string"}, "principal": {"type": "string"}, "action": {"type": "string"}, "resource_type": {"type": "string"}, "condition": {"type": "string"}}, "required": ["name", "effect", "principal", "action", "resource_type"]}},
            {"name": "evaluate_policies", "description": "Evaluate all Policy KOs for a (principal, action, resource) tuple.", "inputSchema": {"type": "object", "properties": {"principal": {"type": "string"}, "action": {"type": "string"}, "resource_type": {"type": "string"}}, "required": ["principal", "action", "resource_type"]}},
            {"name": "deploy_workflow", "description": "Deploy a workflow (DAG of programs) as a versioned KO (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "steps": {"type": "array"}}, "required": ["name", "steps"]}},
            {"name": "deploy_trigger", "description": "Deploy an event-condition-action trigger as a versioned KO (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "event_kind": {"type": "string"}, "type_filter": {"type": "string"}, "program_koid": {"type": "string"}}, "required": ["name", "event_kind", "program_koid"]}},
            {"name": "add_dependency", "description": "Create a DEPENDS_ON relationship between two Active KOs (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"source": {"type": "string"}, "target": {"type": "string"}, "dep_type": {"type": "string"}}, "required": ["source", "target"]}},
            {"name": "execute_workflow", "description": "Execute a Workflow KO by KOID — runs all program steps in order (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "check_triggers", "description": "Check journal for matching Trigger KOs and fire them (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "program_cache_stats", "description": "Return ProgramCache hit stats (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_agent", "description": "Deploy an AI agent as a versioned Knowledge Object with prompt, skills, tools, policies (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "prompt": {"type": "string"}, "skills": {"type": "array"}, "tools": {"type": "array"}, "policies": {"type": "array"}}, "required": ["name"]}},
            {"name": "list_agents", "description": "List all deployed agent Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "execute_agent", "description": "Execute an Agent KO — loads the agent, resolves skills to Program KOs, executes each skill (MRFC-0030 Phase 7c).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "deploy_connector", "description": "Deploy an external system connector as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "plugin": {"type": "string"}, "config": {"type": "object"}, "mapping": {"type": "array"}}, "required": ["name", "plugin"]}},
            {"name": "list_connectors", "description": "List all deployed connector Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_view", "description": "Deploy a materialized knowledge view as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "query": {"type": "string"}, "refresh_seconds": {"type": "integer"}}, "required": ["name", "query"]}},
            {"name": "list_views", "description": "List all deployed view Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_report", "description": "Deploy a compliance/analytics report definition as a versioned Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "template": {"type": "string"}, "format": {"type": "string"}, "parameters": {"type": "array"}}, "required": ["name", "template", "format"]}},
            {"name": "list_reports", "description": "List all deployed report Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "deploy_benchmark", "description": "Deploy a versioned, replayable performance benchmark as a Knowledge Object (MRFC-0030).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "target_query": {"type": "string"}, "iterations": {"type": "integer"}, "warmup": {"type": "integer"}}, "required": ["name", "target_query"]}},
            {"name": "list_benchmarks", "description": "List all deployed benchmark Knowledge Objects (MRFC-0030).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "document_ingest", "description": "Ingest a document: base64-encoded content → artifact store → Document KO. Returns koid + SHA-256. (MRFC-0050)", "inputSchema": {"type": "object", "properties": {"filename": {"type": "string"}, "content_base64": {"type": "string"}, "mime_type": {"type": "string"}}, "required": ["filename", "content_base64"]}},
            {"name": "document_list", "description": "List all ingested Document Knowledge Objects (MRFC-0050).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "document_status", "description": "Get processing status and metadata for an ingested document by KOID (MRFC-0050).", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}}, "required": ["koid"]}},
            {"name": "document_compile", "description": "Run the full D1-D9 document knowledge compiler pipeline on an ingested document. Returns IR entities, ontology proposals, entity resolution, commit plan, embedded chunks, and evidence trail. (MRFC-0050)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "compile_context", "description": "Compile a minimum sufficient context package for an agent task from a knowledge document. Takes a task description and returns ranked entities, facts, and relationships under a token budget. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "task": {"type": "string", "description": "Natural language task description"}, "token_budget": {"type": "integer", "default": 2000, "description": "Max tokens for the context package"}, "subject": {"type": "string"}}, "required": ["koid", "task"]}},
            {"name": "reconcile", "description": "Reconcile changed files against a knowledge document. Given a list of changed file paths (e.g., from git diff), returns affected entities, potentially stale facts, and an impact report. (MRFC-0070-A8)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "files": {"type": "array", "items": {"type": "string"}, "description": "List of changed file paths (e.g., from git diff --name-only)"}, "subject": {"type": "string"}}, "required": ["koid", "files"]}},
            {"name": "connector_bridge", "description": "Convert connector schema metadata into KnowledgeIr. Provide connector_type (postgres/sqlite/mongodb/neo4j), label, and optional tables/references arrays. Each table needs name and fields (array of {name, data_type, is_primary_key, nullable, is_unique}). Each reference needs from_container, from_fields, to_container, to_fields, and optional name. (MRFC-0070-A9)", "inputSchema": {"type": "object", "properties": {"connector_type": {"type": "string"}, "label": {"type": "string"}, "tables": {"type": "array"}, "references": {"type": "array"}}, "required": ["connector_type"]}},
            {"name": "filter_secrets", "description": "Scan a knowledge document for secrets, API keys, tokens, emails, credit cards, and PII. Returns a list of findings with type, location, and redacted text. (MRFC-0070-A7)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "explain_component", "description": "Explain a component: purpose, dependencies, dependents, facts, decisions, and tests. aikoql: EXPLAIN COMPONENT. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "name": {"type": "string", "description": "Component name"}, "subject": {"type": "string"}}, "required": ["koid", "name"]}},
            {"name": "explain_decision", "description": "Explain an architectural decision: context, problem, options, selected, rationale, consequences. aikoql: EXPLAIN DECISION. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "name": {"type": "string", "description": "ADR name"}, "subject": {"type": "string"}}, "required": ["koid", "name"]}},
            {"name": "trace_requirement", "description": "Trace a requirement through decisions, components, functions, to tests. aikoql: TRACE REQUIREMENT. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "requirement": {"type": "string", "description": "Requirement text or ID"}, "subject": {"type": "string"}}, "required": ["koid", "requirement"]}},
            {"name": "find_conflicts", "description": "Find contradictory claims and ambiguous facts about a component. aikoql: FIND CONFLICTS. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "component": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid", "component"]}},
            {"name": "find_stale", "description": "Find stale documentation: documentation that has diverged from code. aikoql: FIND STALE DOCUMENTATION. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid"]}},
            {"name": "validate_change", "description": "Validate a proposed change: what knowledge entities, facts, and relations would be affected? Returns risk assessment. aikoql: VALIDATE CHANGE. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "change": {"type": "string", "description": "Change description"}, "subject": {"type": "string"}}, "required": ["koid", "change"]}},
            {"name": "propose_update", "description": "Propose a knowledge update: add/remove facts, update entities, add/remove relations. Enters reconciliation workflow (PROPOSED → VALIDATED → ACCEPTED/REJECTED). aikoql: PROPOSE KNOWLEDGE UPDATE. (MRFC-0070-A6)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "action": {"type": "string", "enum": ["add_fact", "remove_fact", "update_entity", "add_relation", "remove_relation"]}, "target_entity": {"type": "string"}, "new_facts": {"type": "array", "items": {"type": "string"}}, "remove_facts": {"type": "array", "items": {"type": "string"}}, "new_relations": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}}, "justification": {"type": "string"}, "agent_id": {"type": "string"}, "subject": {"type": "string"}}, "required": ["koid", "action"]}},
            {"name": "discover_schema", "description": "Discover all types and their properties in the database (MRFC-0040 agent experience).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "discover_ontology", "description": "Auto-discover an ontology from all stored Knowledge Objects: classes, properties, relationships, and source mappings (MRFC-0041). Saves the ontology as an Ontology KO.", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "health", "description": "Health check with readiness, journal seq, journal lag, object count, connection pool, uptime (MRFC-0040).", "inputSchema": {"type": "object", "properties": {}}},
            {"name": "agent_memory", "description": "Store or retrieve agent memories with TTL. Write: agent_id + key + value. Read: agent_id only. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"agent_id": {"type": "string"}, "key": {"type": "string"}, "value": {}, "ttl": {"type": "integer"}}, "required": ["agent_id"]}},
            {"name": "batch", "description": "Atomic batch of remember/relate/forget operations. Use $N.koid to reference previous results. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"operations": {"type": "array"}}, "required": ["operations"]}},
            {"name": "session_init", "description": "Establish agent session identity. Subsequent calls in this connection inherit agent_id, run_id, tenant, roles. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"agent_id": {"type": "string"}, "run_id": {"type": "string"}, "tenant": {"type": "string"}, "roles": {"type": "array", "items": {"type": "string"}}}, "required": ["agent_id"]}},
            {"name": "decide", "description": "Record an agent decision on a Knowledge Object with rationale and confidence. Creates a provenance-tagged version. (MRFC-0040)", "inputSchema": {"type": "object", "properties": {"koid": {"type": "string"}, "decision": {"type": "string"}, "rationale": {"type": "string"}, "confidence": {"type": "number"}}, "required": ["koid", "decision"]}},
            {"name": "memory_search", "description": "Search the agent memory directory for knowledge fragments. Returns ranked results with name, description, snippet, and relevance. The memory directory contains Markdown files with YAML frontmatter — each file is one memory. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"query": {"type": "string", "description": "Search query — matched against memory names, descriptions, and body content"}, "max_results": {"type": "integer", "default": 10, "description": "Maximum number of results to return"}, "memory_dir": {"type": "string", "description": "Override the memory directory path (default: server --memory-dir)"}}, "required": ["query"]}},
            {"name": "memory_store", "description": "Store a new memory as a Markdown file with YAML frontmatter in the memory directory. Auto-generates the filename from the name slug. The memory is indexed in MEMORY.md. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Short kebab-case slug for this memory (e.g. 'mrf-0070-phase-a1-complete')"}, "description": {"type": "string", "description": "One-line summary used to decide relevance during recall"}, "content": {"type": "string", "description": "Body of the memory — the fact, decision, or knowledge to persist"}, "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"], "default": "project", "description": "Memory type"}, "memory_dir": {"type": "string", "description": "Override the memory directory path (default: server --memory-dir)"}}, "required": ["name", "description", "content"]}},
        {"name": "memory_update", "description": "Update an existing memory's frontmatter fields and/or body content. Only provided fields are changed — omitted fields keep their current values. Updates the modified timestamp. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Name slug of the memory to update"}, "description": {"type": "string", "description": "New one-line summary (omit to keep current)"}, "content": {"type": "string", "description": "New body content (omit to keep current)"}, "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"], "description": "New memory type (omit to keep current)"}, "memory_dir": {"type": "string", "description": "Override the memory directory path"}}, "required": ["name"]}},
        {"name": "memory_delete", "description": "Delete a memory file from the memory directory and remove its entry from MEMORY.md. Returns the deleted memory's name and path. (MRFC-0070)", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Name slug of the memory to delete"}, "memory_dir": {"type": "string", "description": "Override the memory directory path"}}, "required": ["name"]}},
        {"name": "record_experience", "description": "Record an agent execution outcome as an aikoql:experience Knowledge Object — agent_derived authority, evidence required, TTL-bounded validity, confidence context. shared_with grants other agents read (reuse) access. (v0.3 K5)", "inputSchema": {"type": "object", "properties": {"subject": subj, "goal": {"type": "string", "description": "What the run was trying to achieve"}, "action": {"type": "string", "description": "What the agent did"}, "outcome": {"type": "string", "description": "What happened"}, "actor": {"type": "string", "description": "Defaults to the calling subject"}, "preconditions": {"type": "array", "items": {"type": "string"}}, "causal_explanation": {"type": "string"}, "lesson": {"type": "string"}, "reuse_conditions": {"type": "array", "items": {"type": "string"}, "description": "Tokens that must ALL appear in a future task before this experience is reused; empty = reuse on goal overlap"}, "evidence": {"type": "array", "items": {"type": "object", "properties": {"source_artifact": {"type": "string"}, "method": {"type": "string"}, "location": {"type": "string"}, "revision": {"type": "string"}, "confidence": {"type": "number"}}, "required": ["source_artifact", "method"]}}, "confidence": {"type": "number", "description": "0..1, default 0.5"}, "ttl_seconds": {"type": "integer", "description": "Default 30 days"}, "shared_with": {"type": "array", "items": {"type": "string"}, "description": "Principals allowed to read and reuse this experience"}, "note": {"type": "string"}}, "required": ["goal", "action", "outcome", "evidence"]}},
        {"name": "find_experiences", "description": "Match recorded agent experiences for reuse against a task description — reuse-condition gating, confidence-weighted ranking, expired/invalidated experiences filtered, ACL-scoped to what the caller may read. (v0.3 K5)", "inputSchema": {"type": "object", "properties": {"subject": subj, "task": {"type": "string", "description": "Natural language task description"}, "limit": {"type": "integer", "default": 5}}, "required": ["task"]}}
        ]
    })
}
pub(crate) fn tool_batch(k: &Kernel, args: &J) -> Result<J, String> {
    let ops = args
        .get("operations")
        .and_then(|v| v.as_array())
        .ok_or("missing: operations")?;
    let mut results = Vec::new();
    let mut koids: Vec<String> = Vec::new();
    for op in ops {
        let name = op
            .get("op")
            .or_else(|| op.get("type"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fallback: detect operation by presence of remember/relate/forget keys
                op.get("remember")
                    .or_else(|| op.get("relate"))
                    .or_else(|| op.get("forget"))
                    .map(|_| {
                        if op.get("remember").is_some() {
                            "remember"
                        } else if op.get("relate").is_some() {
                            "relate"
                        } else if op.get("forget").is_some() {
                            "forget"
                        } else {
                            "unknown"
                        }
                    })
            })
            .unwrap_or("unknown");
        // Substitute $N references with previously returned KOIDs.
        let op_str = op.to_string();
        let mut resolved = op_str.clone();
        for (i, koid) in koids.iter().enumerate() {
            resolved = resolved.replace(&format!("${}.koid", i + 1), koid);
        }
        let resolved_op: J = serde_json::from_str(&resolved).unwrap_or(op.clone());
        let r: Result<J, String> = match name {
            "remember" => tool_remember(k, &resolved_op),
            "relate" => tool_relate(k, &resolved_op),
            "forget" => tool_forget(k, &resolved_op),
            _ => Err(format!("unknown batch op: {}", name)),
        };
        match r {
            Ok(result) => {
                if let Some(koid) = result.get("koid").and_then(|v| v.as_str()) {
                    koids.push(koid.to_string());
                }
                results.push(json!({"op": name, "ok": true, "result": result}));
            }
            Err(e) => {
                results.push(json!({"op": name, "ok": false, "error": e}));
            }
        }
    }
    Ok(json!({"results": results, "count": results.len()}))
}
pub(crate) fn call_tool(
    k: &Kernel,
    name: &str,
    args: &J,
    db_path: &str,
    session: &mut McpSession,
) -> ToolResult {
    // PRR-2 defense in depth: a TCP session with no roles can never pass the
    // authz empty-roles passthrough. Startup rejects role-less token specs,
    // so this only trips if that invariant is broken — fail closed anyway.
    if session.trust_mode == TrustMode::Tcp && session.roles.is_empty() {
        audit_log(
            db_path,
            &session.agent_id,
            name,
            "denied:no-roles",
            "TCP session has no roles",
        );
        return Err((
            -32004,
            "TCP session has no roles — configure --tcp-token with at least one role".into(),
        ));
    }
    // A7: Check capability + rate limit before dispatch
    if let Err(e) = check_capability(&session.roles, name) {
        audit_log(db_path, &session.agent_id, name, "denied:capability", &e.1);
        return Err(e);
    }
    if let Err(e) = check_rate(&session.agent_id, &session.roles, 120) {
        audit_log(db_path, &session.agent_id, name, "denied:rate", &e.1);
        return Err(e);
    }

    let res = match name {
        "remember" => tool_remember(k, args),
        "forget" => tool_forget(k, args),
        "evolve" => tool_evolve(k, args),
        "transition_epistemic" => tool_transition_epistemic(k, args),
        "derive" => tool_derive(k, args),
        "observe" => tool_observe(k, args),
        "assert_knowledge" => tool_assert_knowledge(k, args),
        "verify_knowledge" => tool_verify_knowledge(k, args),
        "contradict" => tool_contradict(k, args),
        "supersede" => tool_supersede(k, args),
        "merge" => tool_merge(k, args),
        "invalidate" => tool_invalidate(k, args),
        "resolve_conflict" => tool_resolve_conflict(k, args),
        "resolve_conflict_by_authority" => tool_resolve_conflict_by_authority(k, args),
        "record_experience" => tool_record_experience(k, args),
        "find_experiences" => tool_find_experiences(k, args),
        "verify" => tool_verify(k, args),
        "get" => tool_get(k, args),
        "find_similar" => tool_find_similar(k, args),
        "trace" => tool_trace(k, args),
        "explain" => tool_explain(k, args),
        "prove" => tool_prove(k, args),
        "provenance" => tool_provenance(k, args),
        "relate" => tool_relate(k, args),
        "traverse" => tool_traverse(k, args),
        "eval_recall" => tool_eval_recall(k, args),
        "eval_staleness" => tool_eval_staleness(k, args),
        "eval_contradictions" => tool_eval_contradictions(k, args),
        "aikoql" => tool_aikoql(k, args),
        "backup" => tool_backup(k, db_path),
        "verify_backup" => tool_verify_backup(args),
        "restore" => tool_restore(args, db_path),
        "list_backups" => tool_list_backups(),
        "metrics" => tool_metrics(k),
        "audit_report" => tool_audit_report(k),
        "compliance_report" => tool_compliance_report(k),
        "reason" => tool_reason(k, args),
        "infer" => tool_infer(k, args),
        "predict" => tool_predict(k, args),
        "abi_version" => tool_abi_version(k),
        "deploy_program" => tool_deploy_program(k, args),
        "execute_program" => tool_execute_program(k, args),
        "list_programs" => tool_list_programs(k, args),
        "deploy_policy" => tool_deploy_policy(k, args),
        "evaluate_policies" => tool_evaluate_policies(k, args),
        "deploy_workflow" => tool_deploy_workflow(k, args),
        "deploy_trigger" => tool_deploy_trigger(k, args),
        "add_dependency" => tool_add_dependency(k, args),
        "execute_workflow" => tool_execute_workflow(k, args),
        "check_triggers" => tool_check_triggers(k),
        "program_cache_stats" => tool_program_cache_stats(),
        "deploy_agent" => tool_deploy_agent(k, args),
        "list_agents" => tool_list_agents(k, args),
        "execute_agent" => tool_execute_agent(k, args),
        "deploy_connector" => tool_deploy_connector(k, args),
        "list_connectors" => tool_list_connectors(k, args),
        "deploy_view" => tool_deploy_view(k, args),
        "list_views" => tool_list_views(k, args),
        "deploy_report" => tool_deploy_report(k, args),
        "list_reports" => tool_list_reports(k, args),
        "deploy_benchmark" => tool_deploy_benchmark(k, args),
        "list_benchmarks" => tool_list_benchmarks(k, args),
        "document_ingest" => tool_document_ingest(k, args, db_path),
        "document_list" => tool_document_list(k, args),
        "document_status" => tool_document_status(k, args),
        "document_compile" => tool_document_compile(k, args, db_path),
        "compile_context" => tool_compile_context(k, args, db_path),
        "reconcile" => tool_reconcile(k, args, db_path),
        "connector_bridge" => tool_connector_bridge(k, args),
        "filter_secrets" => tool_filter_secrets(k, args, db_path),
        "explain_component" => tool_explain_component(k, args, db_path),
        "explain_decision" => tool_explain_decision(k, args, db_path),
        "trace_requirement" => tool_trace_requirement(k, args, db_path),
        "find_conflicts" => tool_find_conflicts(k, args, db_path),
        "find_stale" => tool_find_stale(k, args, db_path),
        "validate_change" => tool_validate_change(k, args, db_path),
        "propose_update" => tool_propose_update(k, args, db_path),
        "discover_schema" => tool_discover_schema(k),
        "discover_ontology" => tool_discover_ontology(k),
        "health" => tool_health(k),
        "agent_memory" => tool_agent_memory(k, args),
        "memory_search" => tool_memory_search(args),
        "memory_store" => tool_memory_store(args),
        "memory_update" => tool_memory_update(args),
        "memory_delete" => tool_memory_delete(args),
        "batch" => tool_batch(k, args),
        "session_init" => tool_session_init(args, session),
        "decide" => tool_decide(k, args),
        _ => Err(format!("unknown tool: {}", name)),
    };
    let wrapped = error_codes::wrap_result(res);
    if wrapped["ok"] == true {
        audit_log(
            db_path,
            &session.agent_id,
            name,
            "ok",
            &tool_detail(name, args),
        );
        Ok(json!({
            "content": [{"type": "text", "text": wrapped["data"].to_string()}],
            "isError": false
        }))
    } else {
        let err_detail = wrapped["error"].as_str().unwrap_or("unknown error");
        audit_log(db_path, &session.agent_id, name, "error", err_detail);
        Ok(json!({
            "content": [{"type": "text", "text": wrapped.to_string()}],
            "isError": true
        }))
    }
}
