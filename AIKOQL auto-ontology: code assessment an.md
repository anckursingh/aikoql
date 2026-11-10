AIKOQL auto-ontology: code assessment and recommended direction
I checked the latest PR listing and the current repository code rather than treating this as a greenfield feature.

The latest PR is #31 — ClickHouse column in the competitor benchmark harness. It adds ClickHouse to the Python benchmarking harness; it does not add a ClickHouse database connector or automatic schema import. The latest Studio and ontology work is already present in the current codebase, including the Auto-Discover workflow and ontology hot-reload.

The important distinction is that AIKOQL already has much of the foundation for what you describe: database introspection, import connectors, a common ingestion path into Knowledge Objects, ontology discovery, and Studio review/save. The opportunity is to join these into a single database onboarding and schema-to-knowledge pipeline, not to build another connector framework or another ontology engine.
1. What is already implemented
The repository has three distinct pieces that are relevant. They are not yet a single end-to-end Studio workflow.
Component	Current status	What the code actually does
PostgreSQL connector	Implemented	Introspects tables, columns, SQL types, nullability and primary keys; imports rows as KOs; can link supported foreign keys.
MongoDB connector	Implemented	Lists collections, samples documents to infer field names, imports BSON documents as KOs, preserves nested maps and arrays.
Neo4j connector	Implemented	Discovers labels and relationship types, samples label properties, imports nodes and relationships.
Connector-to-IR bridge	Implemented	Converts connector metadata into KnowledgeIr: containers become entities, fields become facts, references become relations.
Ontology proposal generation	Implemented	Converts KnowledgeIr into proposed classes, properties and relationships, including typed properties from connector schema facts.
Multi-source ontology discovery	Tested	ont001_live_discovery_merges_three_sources exercises live PostgreSQL, MongoDB and Neo4j introspection through the bridge and merges their proposals.
Studio Auto-Discover	Implemented	Discovers ontology from KOs already stored in AIKOQL, previews it, and saves it after explicit confirmation.
Live database onboarding from Studio	Not integrated	I did not find a Studio workflow that connects to a live external database, introspects it, imports its data and generates its ontology as one operation.
ClickHouse provider	Not found	ClickHouse is present in the benchmark harness, not as a database import/introspection connector.

Relevant implementation:
- PostgreSQL provider
- MongoDB provider
- Neo4j provider
- Connector bridge
- Ontology discovery
- Connector certification tests
This is a substantial head start. In particular, the live three-source test is important: it proves that the basic schema-to-IR-to-ontology transformation is not just a theoretical design.
2. The existing flow, and where it stops
The current architecture can be represented as follows.
Current connector and ontology pipeline
External databases

PostgreSQL · MongoDB · Neo4j · SQLite


Provider connectors

Connect · introspect · import records


Knowledge Objects

Rows/documents become KOs with source metadata


Connector Bridge

ConnectorMetadata → KnowledgeIr


Ontology discovery

KnowledgeIr → classes, properties, relationships


Ontology Knowledge Object

Validated and registered for compiler use

Important: the live three-source path is demonstrated in certification tests. Studio's existing Auto-Discover instead starts from stored KOs. The production orchestration that joins these paths is the missing integration.
There are two separate flows today:

Flow A — live connector discovery in the certification test

The test performs:
1. Connect to PostgreSQL, MongoDB and Neo4j.
2. Call each provider's introspection methods.
3. Convert the returned metadata into ConnectorMetadata.
4. Call connector_metadata_to_ir().
5. Call discover_ontology_from_ir().
6. Merge proposals with merge_proposals().
This is verified in ont001_live_discovery_merges_three_sources.

Flow B — Studio Auto-Discover

The Studio button calls the ontology discovery endpoint. tool_discover_ontology() scans stored Knowledge Objects, calls the kernel's discover_ontology() and saves the generated ontology when requested.

The Studio correctly previews before saving. The active ontology is also refreshed without restarting the server.

Sources: agent_knowledge.rs, api_rest.rs, Studio app.js.

My conclusion: the pieces exist, but Studio is not yet using the live connector-to-IR pipeline. Connecting those two flows is the main engineering task.
3. The important issue: current ontology discovery is not yet a full domain ontology generator
I found a limitation worth addressing before exposing this as a major product capability.

In connector_metadata_to_ir(), the entity type is generated using:
type_hint: Some(format!("{}{}", meta.connector_type, container_type))


For example:
- PostgreSQL tables become postgresTable.
- MongoDB collections become mongodbCollection.
- Neo4j labels become neo4jNodeLabel.
The ontology discovery function uses the entity's type_hint as the proposed class name.

This means that multiple PostgreSQL tables can collapse into one proposed class: postgresTable. Likewise, multiple MongoDB collections can collapse into mongodbCollection.

That is useful for recognizing the source's structural type, but it is not what I would consider a proper domain ontology.

Consider a database with:
customers
orders
products
payments


The ontology should be able to propose:
Customer
Order
Product
Payment


with their individual properties and relationships.

It should not simply produce:
PostgresTable
MongoCollection


The existing test verifies that source container types are discovered and that typed properties and relationships are produced. It does not establish that semantically meaningful business classes are inferred.
My recommendation
Separate three concepts:
Concept	Example	Purpose
Physical source type	postgres.public.customers	Identifies the actual source object
Canonical ontology class	Customer	Represents the domain concept
Knowledge Object type	Customer	Represents individual customer records

The ontology mapping should then explicitly connect them:
postgres.public.customers ──maps to──> Customer
mongodb.crm.clients       ──maps to──> Customer


The property mappings can normalize different physical names:
postgres.customers.customer_id → Customer.id
mongodb.clients._id            → Customer.id
postgres.customers.email        → Customer.email
mongodb.clients.email_address   → Customer.email


This is how AIKOQL can create a common knowledge model from heterogeneous databases, rather than merely importing several unrelated data sources.
4. What each connector needs before this is production-ready
The current connectors are useful, but their metadata completeness differs.
PostgreSQL
Strong foundation
Already present:
- Table and column discovery.
- PostgreSQL type information.
- Nullability and primary-key detection.
- Foreign-key introspection.
- Row import and supported FK-to-RelationshipRef linking.
Limitations found:
- TableSchema does not currently carry a complete set of unique constraints and indexes.
- Composite foreign keys are explicitly limited: the provider retains only the first column pair for a constraint.
- FK linking supports single-column primary keys and tables imported in the same run.
Recommended enhancement: expose complete constraint metadata, including composite keys, unique constraints, indexes, check constraints and schema namespace. Preserve the actual database/schema/table identity rather than relying on a table name alone.
MongoDB
Needs richer schema inference
Already present:
- Collection discovery.
- Document sampling, currently limited to 100 documents.
- Nested property-name discovery.
- BSON-to-KO conversion preserving nested maps and arrays.
- Deterministic identity based on _id.
Limitations:
- CollectionSchema stores property names, not a complete field-level type/nullability/constraint model.
- A field appearing in one sampled document does not establish that it is universally present.
- The existing ontology test supplies "bson" as the field data type, rather than an inferred concrete type.
- Different document shapes within one collection are not fully represented by a single schema.
Recommended enhancement: infer a field profile over a configurable sample, such as:
field: profile.age
observed_types: [Int, Null]
presence_ratio: 0.91
null_ratio: 0.04
sample_count: 1000


Represent heterogeneous types as unions or variants, not as a forced single type. Distinguish observed schema from enforced schema.
Neo4j
Needs richer graph schema discovery
Already present:
- Label discovery.
- Relationship-type discovery.
- Property-name discovery.
- Node and relationship import.
- Relationship references represented in KOs.
Limitations:
- introspect_label_props() samples only one node. Its source comment explicitly acknowledges that heterogeneous labels may under-report properties.
- Property data types and constraints are not comprehensively introspected.
- The current ontology test does not establish full domain/range inference for every relationship type.
Recommended enhancement: inspect properties across a representative sample of nodes per label, and infer relationship domain/range from actual graph patterns. Capture uniqueness constraints and indexes where available.
ClickHouse
Provider not found
PR #31 adds ClickHouse to scripts/competitor_bench/bench.py. That is a benchmark integration, not a database connector.

I would add a dedicated provider with metadata discovery from ClickHouse system tables, including:
- Database and table names.
- Column names and types.
- Nullable and low-cardinality types.
- Engine and sorting key.
- Primary key and partition key.
- Materialized views.
- Table-level metadata and supported constraints.
ClickHouse should initially support schema discovery and controlled read-only ingestion. It is an analytical database, so large-table ingestion needs streaming/batching and explicit snapshot semantics rather than loading entire tables into memory.
5. The architecture I recommend
I would build this as an Auto Schema & Ontology Discovery pipeline, reusing the existing provider, bridge, IR and ontology code.

I would not create a new ontology engine.
Proposed AIKOQL database onboarding
1. Source registration

Studio → Add database → select connector → authenticate → test connection


2. Schema introspection

Provider-specific metadata → normalized ConnectorMetadata

Tables · columns · keys · constraints · indexes · relationships · types


3. Schema Knowledge Objects

Persist source, database, table, field, constraint and relationship metadata as versioned KOs.


4. Existing Connector Bridge + Knowledge IR

Reuse connector_metadata_to_ir() and extend its metadata fidelity.


5. Ontology proposal and entity resolution

Infer domain classes, typed properties, relationships and cross-source mappings.


6. Studio review and publication

Preview → inspect evidence → resolve conflicts → approve → publish ontology.


7. Data ingestion and mapping

Import records as KOs, apply canonical mappings, retain source identity and provenance.


5.1 Schema Knowledge Objects
I recommend persisting the source schema itself as knowledge, rather than keeping it only in connector memory or in an ontology document.

For example:
KO type	Example
aikoql:source	PostgreSQL production connection
aikoql:database	commerce
aikoql:table	public.customers
aikoql:field	public.customers.customer_id
aikoql:constraint	customers_pkey
aikoql:source_relationship	orders.customer_id → customers.id
Ontology	Canonical commerce ontology

These would be ordinary KOs using the existing kernel, storage, versioning, tenant and provenance mechanisms.

A schema KO should retain:
- Stable source ID and physical object ID.
- Connector type and source database.
- Native type and normalized AIKOQL type.
- Nullability, primary-key and uniqueness information.
- Constraint and relationship references.
- Source schema version or fingerprint.
- Observation timestamp and discovery run.
- Provenance back to the source database.
This provides a useful capability beyond ontology generation: AIKOQL can query the evolution of external database schemas over time.
5.2 Canonical ontology generation
The ontology generator should combine three kinds of evidence:
1. Structural evidence: tables, fields, types, constraints, foreign keys, graph relationships.
2. Data-profile evidence: observed values, cardinality, null ratios, distributions and nested shapes.
3. Semantic evidence: table names, column names, comments, domain vocabulary and cross-source identity.
The first two can be deterministic. The third can use optional model-assisted suggestions.

For example, if a PostgreSQL table has customer_id, email, created_at, and MongoDB has client_id, email_address, registered_on, the system can propose that both represent the same domain concept.

But it must not automatically merge them just because their names look similar. It should retain evidence and confidence, and ask for approval when the match is ambiguous.
5.3 Do not confuse schema discovery with data ingestion
This distinction matters for both product design and security.

A user connecting a database should be able to choose:
- Discover schema only: read metadata, generate schema KOs and ontology proposals. No business records copied.
- Discover and sample: additionally inspect a bounded number of records for data profiling and semantic inference.
- Discover and ingest: import selected tables or collections into AIKOQL as KOs.
Schema-only should be the default.

It makes the onboarding experience safer and allows AIKOQL to serve as a knowledge layer over databases without necessarily duplicating all their data.
6. How Studio should evolve
I would extend the existing Studio rather than add a separate interface.
Proposed Studio experience
Data Sources
Proposed UI

PostgreSQL

Connected

MongoDB

Connected

Add source

Connector



Discovery configuration

Example configuration, not a live connection
- Source: commerce-postgres
- Scope: public schema
- Mode: Schema + sample
- Sample limit: 1,000 records per table
- Include relationships: Yes
  
  Discovery result
  12
  Tables
  
  84
  Fields
  
  9
  Relationships
  
  7
  Proposed classes
  
  
  
  Ontology review
  Customer
  High confidence
  
  Mapped from public.customers and crm.clients
  Order
  High confidence
  
  Mapped from public.orders
  Account
  Review required
  
  Possible match across two source systems
  
  Preview first · Publish only after approval

The existing Ontology panel already has preview/save behavior and tenant-aware ontology endpoints. I would add a Data Sources panel and have the ontology panel display the sources and evidence behind each proposed mapping.

The UI should also make it possible to inspect the physical schema, the canonical ontology and the mapping between them.
7. The most important engineering decisions
There are a few decisions I would make before implementation.

A. Keep source identity separate from canonical identity.

Two source records can map to one canonical entity, but their original IDs must remain recoverable. Do not replace source identity with a canonical KOID.

B. Schema changes must not silently overwrite ontology decisions.

When a database schema changes, AIKOQL should create a new schema snapshot and a change proposal. For example:
PostgreSQL schema v1
  customers.email: varchar

PostgreSQL schema v2
  customers.email: varchar
  customers.phone: varchar


The system should detect the new field, update the source schema KOs and propose an ontology change. It should not silently rewrite a manually approved ontology.

C. Distinguish observed from declared constraints.

PostgreSQL constraints can be authoritative declarations. MongoDB inferred types are observations. Neo4j sampled properties are observations. These should not all receive identical confidence or authority.

D. Use read-only source credentials for discovery.

Schema discovery should not require write privileges on the external databases. Credentials should be managed through the existing secret/configuration facilities, not persisted in ordinary KOs or logs.

E. Keep tenant isolation at the connector and ontology levels.

A source connection, its schema KOs, discovery runs, mappings and active ontology must all belong to a tenant. The Studio's tenant selection must not become an authorization substitute.

8. Implementation plan
I would split this into five focused PRs. Most of the foundational work is already present, so this is primarily integration and enrichment.
PR	Scope	Priority
1	Production ConnectorMetadata adapters: reuse live provider introspection rather than manually constructing metadata	P0
2	Persist source/schema/field/constraint KOs with stable source identity and version fingerprints	P1
3	Improve ontology generation: per-container domain classes, typed properties, relationship domains/ranges, cross-source identity proposals	P1
4	Studio Data Sources workflow: connect, discover, preview, approve and publish	P1
5	ClickHouse provider and schema discovery, followed by profiling and controlled ingestion	P2

I would also add a single end-to-end test that exercises the actual production workflow:
PostgreSQL + MongoDB
        |
        v
Register sources
        |
        v
Introspect both
        |
        v
Persist schema KOs
        |
        v
Generate one merged ontology proposal
        |
        v
Review and approve
        |
        v
Import selected records
        |
        v
Query canonical Customer across both sources


This test must run through the same production services that Studio invokes. The current ONT-001 test is valuable, but it is not a substitute for that full integration test.
9. My overall assessment
Yes, AIKOQL can become a system where a user connects PostgreSQL, MongoDB, ClickHouse and other databases and AIKOQL automatically builds a unified ontology and schema knowledge base.

In fact, the existing code already contains the essential building blocks:
- Real database introspection.
- Import to Knowledge Objects.
- Connector metadata normalization.
- Knowledge IR.
- Typed ontology proposals.
- Multi-source proposal merging.
- Ontology persistence and hot-reload.
- Studio preview and explicit publication.
The key missing pieces are production orchestration, richer source metadata, domain-level class inference and the Studio onboarding experience.

One strategic point: I would position this as Universal Database Knowledge Discovery, not merely automatic schema generation. A conventional schema crawler tells users what tables and columns exist. AIKOQL should go further: understand what those objects represent, identify relationships across systems, preserve the source evidence and expose the result as queryable knowledge.

That would make AIKOQL useful even when the original databases remain the systems of record and their data is not fully copied into AIKOQL.

My recommended first implementation is PR 1 + PR 2: connect the existing live introspection code to a production service and persist schema metadata as KOs. Once this works, the ontology generator and Studio workflow can build on real, versioned source metadata instead of relying on manually assembled bridge input.s