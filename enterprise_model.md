Yes. The part of Neon’s model worth borrowing is not simply “give PostgreSQL away for free.” It is the **developer-to-enterprise growth loop**:

1. Make the core database easy and free to adopt.
2. Remove infrastructure and operational friction through a managed service.
3. Let usage grow naturally.
4. Monetise production scale, reliability, governance, collaboration and support—not basic access to the database.

For AIKOQL, I would adapt that model rather than copy Neon’s pricing or infrastructure. AIKOQL has a different value proposition: it can offer a free knowledge database and agent-memory foundation, then charge for managed operation and enterprise controls.

I’ve found an important distinction in the current repository: its documented tier design is presently **Community versus Enterprise**, with an offline-verified licence and a managed tier intended to consume the Enterprise edition. I’m checking the actual implementation and tests now, because the commercial recommendation depends on what is already enforced in code versus what is only specified in documentation.

## **1. What Neon is doing—and what AIKOQL should learn from it**

[The best free database providers in 2024 - Noah Falk](https://images.openai.com/static-rsc-4/tJeYusziZ-YVcebuSwhIiHOsCGoKwYF0gZsaAAcW57ob6qg1NPZ_1rDbDFhB47qMtwM8PEUjKBFxSo7B2HUO4S8P3OM5hDdoxK1l5jw4l47xnv9oQLmZ9XUrOWP7_d1Xye_vZKeZU8B8nWHtwXCYY9qaZWlDyEZzl0JWA3vmFLw?purpose=inline)

**Neon**

Its model is now broader than hosted PostgreSQL: projects, database branches, compute, object storage, functions and authentication are packaged as one developer backend.

Sources: [Neon plans](https://github.com/neondatabase/website/blob/main/content/docs/introduction/plans.md?utm_source=chatgpt.com) · [Neon backend](https://neon.com/blog/neon-backend-is-ga?utm_source=chatgpt.com)

Neon's current Free plan is deliberately generous: 100 projects, 100 compute-unit hours per project per month, 1 GB database storage per project, 10 branches and 5 GB network transfer per project. Compute scales to zero after inactivity. Its paid Launch plan charges by compute time and storage, while Scale adds enterprise-grade capabilities.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub+2



There are four strategic lessons here:

| Neon principle                         | AIKOQL equivalent                                                       |
| -------------------------------------- | ----------------------------------------------------------------------- |
| One project per application            | One AIKOQL knowledge workspace per application or agent                 |
| Free database encourages adoption      | Free hosted knowledge database encourages agent developers              |
| Usage-based compute and storage        | Charge for active compute, persistent storage and ingestion             |
| Production capabilities drive upgrades | Charge for governance, security, collaboration, reliability and support |

The most interesting part is that Neon is not merely selling database capacity. It is becoming the default backend that developers provision whenever they start a new project. **AIKOQL should aim to become the default knowledge and memory backend whenever developers build an AI agent.**

There is one important difference: AIKOQL should not try to become another general-purpose cloud platform immediately. Its differentiator is the knowledge layer—structured Knowledge Objects, provenance, temporal knowledge, hybrid retrieval and persistent agent memory.

## **2. What your current repository actually implements**

I inspected the default branch at commit `adbecd63a790f8b5890874916dde277f3c3af5fe`, including the tier documentation, licence implementation, API and tests.

The current commercial design is:

**Community**

Free · AGPL-3.0-only

Full community server, without a licence.

**Enterprise**

Offline licence

OIDC SSO, multi-user roles and audit retention/export.

**Managed**

Commercial service model, not a separate licence tier

The documentation says `cloud.aikoql.com` will run the same Enterprise edition.

The implementation is more specific than a generic Community/Business distinction:

| Area                 | Current repository evidence                     | Commercial implication                         |
| -------------------- | ----------------------------------------------- | ---------------------------------------------- |
| Community database   | Full server remains available without a licence | Good open-source adoption strategy             |
| Enterprise licensing | Ed25519 signed licence, verified offline        | Useful for on-premise commercial sales         |
| SSO                  | OIDC                                            | Enterprise feature already gated               |
| User management      | SQLite user store, `admin` and `viewer` roles   | Basic multi-user capability                    |
| Audit                | Retention and JSONL export                      | Enterprise differentiator                      |
| Billing              | No billing or metering in the tier design       | Must be built for cloud                        |
| Quotas               | No plan-based usage quotas in the tier design   | Must be built for cloud                        |
| Multi-tenancy        | Explicitly excluded from the tier design        | Cloud control plane and isolation still needed |
| HA with multi-user   | Documented as unsupported in v1                 | Important production limitation                |

The most important finding is in the design specification itself: billing, metering and quotas are explicit non-goals of the current Enterprise tier design. It also says managed deployments are per-customer isolated instances, with a separate control plane. [Enterprise tier design](https://github.com/anckursingh/aikoql/blob/adbecd63a790f8b5890874916dde277f3c3af5fe/docs/superpowers/specs/2026-11-03-enterprise-tiers-design.md?utm_source=chatgpt.com).

The current licence feature set is only:

- `oidc-sso`
- `multi-user`
- `audit-export`

There is no `business` plan object, subscription lifecycle, usage ledger, quota enforcement or cloud billing implementation in this design. The tests confirm that unlicensed servers report `community`, and that licensed servers report `enterprise`. [Licence implementation](https://github.com/anckursingh/aikoql/blob/adbecd63a790f8b5890874916dde277f3c3af5fe/crates/services/api/mcp/src/license.rs?utm_source=chatgpt.com) · [Admin API tests](https://github.com/anckursingh/aikoql/blob/adbecd63a790f8b5890874916dde277f3c3af5fe/crates/services/api/mcp/tests/admin_api.rs?utm_source=chatgpt.com).

### **A subtle but important storage distinction**

AIKOQL already has S3-compatible infrastructure code. Its `s3.rs` implements SigV4 requests and is used by the HA lease and archive paths. The documentation also mentions S3, MinIO and Cloudflare R2 for managed deployments.

However, that is not yet equivalent to Neon's complete, customer-facing S3-compatible object-storage product. The current code does not establish a general-purpose object-storage service with user buckets, object APIs, per-tenant quotas, billing and lifecycle management.

I would not advertise that as an existing AIKOQL capability yet. [S3 implementation](https://github.com/anckursingh/aikoql/blob/adbecd63a790f8b5890874916dde277f3c3af5fe/crates/services/api/mcp/src/s3.rs?utm_source=chatgpt.com).

**My assessment:** your current Community/Enterprise split is a reasonable starting point for selling self-hosted software. It is not yet the right commercial architecture for a Neon-style cloud business.

## **3. The business model I recommend for AIKOQL**

I would introduce four commercial offerings, rather than trying to force everything into the current two-tier licence structure.

**1. AIKOQL Community**

Free

# **$0**

**Who:** Developers, researchers, open-source users and self-hosters.

- Full core database and query engine.
- Agent memory and MCP integration.
- Local deployment using Docker or binaries.
- SDKs and community support.
- No artificial limits on core database features.
- AGPL-3.0-only.

**2. AIKOQL Cloud Free**

Developer acquisition

# **$0**

**Who:** Developers building their first agents and prototypes.

- Managed AIKOQL instance.
- One or a few knowledge workspaces.
- Limited persistent storage.
- Limited ingestion and retrieval capacity.
- Hosted MCP endpoint.
- Basic dashboard and monitoring.
- Automatic suspension or scale-to-zero when idle, if the deployment architecture supports it reliably.

**3. AIKOQL Cloud Pro / Business**

Primary revenue engine

# **$20–$99+**

Illustrative starting-price hypothesis, not a validated price list.

**Who:** Startups, agent developers and SaaS companies.

- Higher compute and storage limits.
- Multiple workspaces and environments.
- Higher ingestion throughput.
- Persistent and configurable compute.
- Backups and restore.
- Usage analytics and cost controls.
- API and MCP access.
- Team collaboration and access controls.

**4. AIKOQL Enterprise**

Sales-led

# **Custom pricing**

**Who:** Enterprises, banks, fintechs and regulated organisations.

- Self-hosted or dedicated managed deployments.
- OIDC SSO and advanced RBAC.
- Audit retention and export.
- HA, disaster recovery and advanced backups.
- Private networking and deployment options.
- Governance, compliance and security features.
- SLA, support and onboarding.
- Commercial licensing and contractual commitments.

I would keep Community and Cloud Free separate. Community is the open-source product. Cloud Free is a subsidised managed service with resource limits. This distinction is important because a self-hosted customer should not need a cloud subscription to use the database.

## **4. What should AIKOQL charge for?**

This is where I would be quite deliberate. Do not charge primarily for the number of Knowledge Objects or agent memory records. That would make the product less attractive precisely when customers start using it successfully.

Instead, charge for the infrastructure and services consumed.

| Meter               | What it measures                              | Commercial treatment                 |
| ------------------- | --------------------------------------------- | ------------------------------------ |
| Compute             | CPU and memory consumption over time          | Usage-based                          |
| Storage             | Persistent database storage, GB-month         | Usage-based                          |
| Object storage      | Original documents, attachments and artifacts | Usage-based                          |
| Ingestion           | Processing and indexing workloads             | Included allowance, then usage-based |
| Network             | Data transferred out                          | Included allowance, then usage-based |
| Backups             | Retention and backup storage                  | Plan allowance or usage-based        |
| Dedicated capacity  | Reserved compute and isolation                | Monthly subscription                 |
| Enterprise features | SSO, governance, HA, support                  | Subscription or contract             |

For example, a developer might have a knowledge database that is idle for 20 hours per day. Charging for a permanently running server would be unattractive. A usage-based compute model would make AIKOQL more competitive.

**One recommendation: make basic agent memory and retrieval inexpensive. Monetise the infrastructure needed to operate it reliably at scale.**

Also, don't introduce a charge per MCP tool call as your main pricing unit. Agent workloads can make that unpredictable. Meter compute and storage underneath, while showing customers a clear estimate of their likely bill.

## **5. How I would evolve the architecture**

I would keep the database engine independent of the commercial control plane. This is essential for preserving AIKOQL's open-source identity.

**AIKOQL Cloud Console**

Projects · Billing · Usage · API keys · Plans

**Cloud Control Plane**

Identity & tenancy

Provisioning

Plan & entitlements

Metering & billing

Quota enforcement

Lifecycle management

**AIKOQL Data Plane**

**AIKOQL Engine**

Storage V2 · KOQL · HNSW

**Agent Interface**

MCP · REST · SDKs

**Ingestion**

Connectors · Extraction · Indexing

**Object Storage**

Documents · Artifacts · Backups

**Infrastructure**

Compute · Persistent volumes · S3-compatible object storage · Monitoring

### **The components I would build**

**A. Organisation and project management**

Introduce a cloud tenancy hierarchy:

```
Organisation
  ├── Members
  ├── Subscription
  ├── Billing account
  └── Projects
       ├── Development
       ├── Staging
       └── Production
            ├── AIKOQL instance
            ├── Storage
            └── Object buckets
```

A project should be the unit of provisioning and billing, much like Neon. It should not be confused with an individual database file or a single agent.

**B. Entitlement and quota service**

This is a missing layer in your current implementation.

I would introduce a central plan definition, independent of the offline licence:

```
struct PlanEntitlements {
    plan_id: String,
    max_projects: u32,
    storage_bytes: u64,
    compute_units: u64,
    ingestion_units: u64,
    egress_bytes: u64,
    max_members: u32,
    features: Vec<String>,
}
```

This is illustrative, not a proposed drop-in implementation. The important design principle is that the cloud control plane owns entitlements, while the data plane enforces them.

For on-premise Enterprise, signed offline licences can continue to work. For Cloud, entitlements should come from the subscription and usage system, not from issuing a new static licence every time a customer changes their plan.

**C. Usage metering**

Every project should generate usage events:

```
compute.started
compute.stopped
storage.sampled
ingestion.completed
network.egress
backup.created
backup.retained
```

Build a durable usage ledger. Calculate bills from that ledger, rather than trying to reconstruct historical consumption from current server metrics.

**D. Object storage**

This is one of the most interesting opportunities.

AIKOQL's knowledge database should store structured knowledge and indexes, while object storage should hold large original documents, images, attachments, raw ingestion artifacts and backups.

I would provide an S3-compatible interface eventually, but start with an internal object-storage abstraction backed by Cloudflare R2, S3 or MinIO.

Do not turn the current S3 archive/lease client into an assumed object-storage product without designing the bucket, object, ACL, lifecycle, tenancy and billing semantics.

## **6. A pricing strategy I would test**

Neon's model works partly because its free tier is generous but its infrastructure is designed around efficient resource consumption. AIKOQL should follow that principle, but its quotas must be based on actual infrastructure costs.

Here is an initial hypothesis to validate against cloud cost measurements:

| Plan           | Monthly price hypothesis | Positioning                            |
| -------------- | ------------------------ | -------------------------------------- |
| Community      | Free                     | Self-hosted                            |
| Cloud Free     | $0                       | Prototypes and experiments             |
| Cloud Pro      | $19–$49                  | Individual developers and small agents |
| Cloud Business | $99–$299                 | Teams and production workloads         |
| Enterprise     | Custom                   | Dedicated and regulated workloads      |

These are starting points for market testing, not recommendations to launch at those prices immediately.

I would offer usage-based upgrades within Pro and Business rather than force customers into a higher fixed plan just because they have a temporary traffic spike.

For the free plan, use hard limits and clear notifications. When a customer reaches a compute allowance, suspend or throttle compute without deleting their data. This is the kind of trust-building behaviour that makes a free tier useful.

## **7. What I would change in your existing Community/Enterprise design**

This is the part I consider most important.

Your current licence design is a feature-gating system. It should not become the entire subscription and billing architecture.

| Existing design                 | Recommended evolution                                 |
| ------------------------------- | ----------------------------------------------------- |
| Community                       | Keep as the free AGPL self-hosted edition             |
| Enterprise                      | Retain as the self-hosted commercial entitlement      |
| `oidc-sso`                      | Retain, with richer identity and organisation support |
| `multi-user`                    | Extend into organisation-level RBAC                   |
| `audit-export`                  | Extend into governance and retention policies         |
| Managed tier                    | Make it a separate cloud subscription model           |
| No billing                      | Introduce usage ledger and billing service            |
| No quotas                       | Introduce plan entitlements and enforcement           |
| Per-customer isolated instances | Evolve into a managed project provisioning system     |

I would also avoid making SSO the only reason to pay. Small teams may need a managed database, backups and increased capacity long before they need enterprise identity federation.

One licensing consideration: AGPL permits commercial hosting, but modified network-accessible versions have source-disclosure obligations under section 13. If you later want proprietary enterprise modules or dual licensing, design that boundary deliberately and obtain legal review. Do not assume that the current AGPL repository and offline feature licence automatically provide a proprietary licensing model.

## **8. Execution roadmap**

I would prioritise the work in this order:

1. **Phase 1 — Commercial foundation**

   Define Organisation, Project, Plan, Subscription, Entitlement and UsageEvent. Keep these outside the database kernel.
2. **Phase 2 — Managed cloud MVP**

   Provision isolated AIKOQL instances, persistent storage, credentials, project lifecycle and basic monitoring.
3. **Phase 3 — Metering and billing**

   Implement usage events, aggregation, quota enforcement, invoices, payment integration and spend alerts.
4. **Phase 4 — Developer experience**

   Cloud console, create-project API, connection strings, API keys, SDK integration and hosted MCP endpoints.
5. **Phase 5 — Production and enterprise**

   Backups, restore, HA, private networking, stronger isolation, SSO, governance, SLAs and support.

I would not start with multi-region HA, a complete S3-compatible public API or a complex billing engine. First prove that developers can create an AIKOQL project, connect an agent, store memory, retrieve it and upgrade when they need more capacity.

## **9. My strategic recommendation**

# **AIKOQL Cloud**

**The managed knowledge infrastructure for AI agents.**

[neon.com](https://neon.com/guides/neon-azure-integration?utm_source=chatgpt.com)

One platform where developers can provision persistent knowledge databases, connect agents, ingest information and scale without managing database infrastructure.

I would build the business around three revenue streams:

1. **Cloud consumption:** compute, storage, ingestion and network usage.
2. **Enterprise subscriptions:** security, governance, dedicated infrastructure, HA and SLAs.
3. **Enterprise support and services:** onboarding, migration, architecture support and contractual commitments.

The flywheel should be:

**Open-source adoption → Cloud Free projects → Real agent workloads → Usage-based revenue → Enterprise conversion.**

The biggest strategic opportunity is to make AIKOQL the knowledge infrastructure layer that sits behind thousands of small agents and applications. You don't need to win the database market in general. You need to become the natural place where agents keep their long-term knowledge.

My next recommended design artifact would be a **Cloud Commercial Architecture RFC** that specifies the tenancy model, control plane, usage ledger, entitlement API, project lifecycle, storage architecture and plan enforcement against the actual Rust codebase. That should be completed before changing the current licence implementation.