# Cognitive Substrate

**An open, model-independent cognitive substrate for AI systems.** It lets them accumulate, validate, revise and act on knowledge over long periods of time, and the whole process stays inspectable, reproducible and under explicit control.

Most "AI memory" is a vector store with a summarizer bolted on. Cognitive Substrate is the layer underneath: knowledge carries provenance, beliefs carry evidence and revision history, time is a first-class dimension, and every recall decision can be replayed.

```
Experience ──► Perception ──► Candidate memory ──► Validation & provenance
                                                          │
       ┌──────────────────────────────────────────────────┘
       ▼
 Knowledge store ── bitemporal revisions · beliefs · evidence · dependency graph
       │
       ▼
 Policy-gated recall ──► context assembly ──► inference / action ──► new experience
       │
       └──► recall traces · cognitive event log · cognitive-debt index
```

## What works today

### Cognitive primitives (M2)

| Capability | What it means |
|---|---|
| **Six memory domains** | Episodic, semantic, procedural, relational, temporal and working memory share one record envelope, and each domain has its own typed payload. |
| **Bitemporal history** | Every revision records *valid time* (when it was true in the world) and *transaction time* (when the substrate knew it). Nothing is overwritten. |
| **Beliefs with evidence** | Propositions carry confidence with an explicit interpretation, plus supporting and refuting evidence and full revision lineage. |
| **Belief dependency graph** | Revising or retracting a belief flags every live belief that relied on it for re-evaluation. Nothing is silently retracted. |
| **Deterministic recall** | Each recall runs under an explicit policy: weighted dense, lexical and recency scoring, scope and time filters, result and token budgets. Ranking is exact and stable. |
| **Recall traces & replay** | Every recall persists its full candidate set. *Exact replay* reproduces a past decision against the substrate as it was. *Counterfactual replay* re-runs it under a different policy. |
| **Cognitive Debt Index** | A deterministic, auditable score for unsupported, contradictory, stale or unverified knowledge, and for knowledge built on invalidated assumptions. |
| **Validation gate** | Model-derived memories enter as `candidate` and must be explicitly promoted before recall treats them as trusted knowledge. |
| **Governance** | Hard scope isolation, logical tombstones, hard purge (including embeddings), and an append-only event log that holds identifiers and never content. |
| **Embedded or standalone** | Use the Rust engine in-process, or run the HTTP service. Both pass the same conformance suite. |

### Governed cognitive processing (M3)

The substrate can now observe, extract, detect and propose. **It still never rewrites knowledge on its own.**

| Capability | What it means |
|---|---|
| **Observations → candidates** | Raw text is stored immutably and queued for perception in one transaction. Extracted facts always enter as `candidate`, linked to their source observation. |
| **Precision-first extraction** | The built-in rule extractor vetoes questions, hedges, conditionals and negations, and requires an anchor token, so small talk yields nothing. An optional OpenAI-compatible extractor (works with Ollama) validates model output strictly. |
| **Durable, fenced jobs** | PostgreSQL `SKIP LOCKED` leases with generation fencing. A job's effects commit atomically with its completion, guarded against read skew, so a stalled worker can never apply stale results. Retries back off, then dead-letter. |
| **Resolution proposals** | Duplicates, supersessions and contradictions become reviewable proposals bound to exact revisions. Accepting one applies it atomically, or marks it `stale` if anything changed. Rejected findings are never re-proposed. |
| **Safe deduplication** | A structural pre-veto stops merges across differing numbers, versions, negation or identifiers. Thresholds are set per embedding model and calibrated against hard negatives. Merges union provenance. |
| **Belief recall** | Recall returns relevant beliefs with their supporting and refuting evidence. Relevance and confidence are never blended. |
| **Purge workflows** | Purging evidence flags dependent beliefs. Purging an observation erases what was derived only from it, and re-homes facts another source corroborates. A purge ledger records what was erased and when, never what it contained. |

## Quick start

### Run the service

```bash
docker compose -f deploy/compose.yaml up --build
```

```bash
TENANT=$(uuidgen); ME=$(uuidgen)
curl -s localhost:8080/v1/memories \
  -H "x-cs-tenant: $TENANT" -H "x-cs-user: $ME" -H 'content-type: application/json' \
  -d '{
    "scope": {"tenant_id": "'$TENANT'", "owner_user_id": "'$ME'", "visibility": "private"},
    "content": {"domain": "semantic", "data": {"proposition": "The project uses PostgreSQL as its authoritative store."}},
    "provenance": {"source": {"type": "user_input"}}
  }'

curl -s localhost:8080/v1/recall \
  -H "x-cs-tenant: $TENANT" -H "x-cs-user: $ME" -H 'content-type: application/json' \
  -d '{"query": "Why did we choose PostgreSQL?"}'
```

Without `DATABASE_URL` the service runs on the in-memory store, which is handy for experiments but not durable.

Feed it raw text and let the background worker extract and resolve:

```bash
curl -s localhost:8080/v1/observations \
  -H "x-cs-tenant: $TENANT" -H "x-cs-user: $ME" -H 'content-type: application/json' \
  -d '{"scope": {"tenant_id": "'$TENANT'", "owner_user_id": "'$ME'", "visibility": "private"},
       "content": "The API depends on the auth service. We decided to freeze deploys on Fridays.",
       "source": {"type": "user_input"}}'

curl -s "localhost:8080/v1/proposals?status=pending" -H "x-cs-tenant: $TENANT" -H "x-cs-user: $ME"
```

To extract with a local model instead of rules, set `CS_EXTRACTOR=openai`, `CS_EXTRACTOR_URL=http://localhost:11434/v1` and `CS_EXTRACTOR_MODEL=<model>`.

### Embed the engine

```rust
use std::sync::Arc;
use cs_core::{AccessContext, Scope, SourceRef, TenantId, UserId};
use cs_engine::{CognitiveEngine, NewMemory, ProvenanceInput, RecallRequest};
use cs_memory::{MemoryContent, SemanticMemory};
use cs_storage::InMemoryStore; // or cs_postgres::PgStore

let engine = CognitiveEngine::new(Arc::new(InMemoryStore::new()));
let (tenant, me) = (TenantId::new_v7(), UserId::new_v7());
let ctx = AccessContext::user(tenant, me);

engine.create_memory(&ctx, NewMemory {
    scope: Scope::private(tenant, me),
    content: MemoryContent::Semantic(SemanticMemory {
        proposition: "Deploys are frozen on Fridays".into(),
        subject: None, predicate: None, object: None,
    }),
    provenance: ProvenanceInput {
        source: SourceRef::UserInput { message_id: None },
        observed_at: None, extraction: None, parent_evidence: vec![],
    },
    valid_time: None, expires_at: None, idempotency_key: None,
}).await?;

let hits = engine.recall(&ctx, RecallRequest {
    query: "can I deploy on friday?".into(),
    policy: Default::default(),
}).await?;
```

## HTTP API (`/v1`)

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health`, `/ready` | Liveness and storage readiness |
| `POST` | `/memories` | Create a memory |
| `GET` | `/memories/{id}` · `/memories/{id}/history` | Current revision · all revisions |
| `POST` | `/memories/{id}/revisions` | Revise (supports `expected_revision`) |
| `POST` | `/memories/{id}/promote` · `/reject` | Validate or reject a candidate |
| `POST` | `/memories/{id}/tombstone` · `/purge` | Logical delete · governance hard delete |
| `POST` | `/recall` | Policy-controlled recall |
| `GET` | `/recall-traces/{id}` | Inspect a recall decision |
| `POST` | `/recall-traces/{id}/replay` | Exact or counterfactual replay |
| `POST` | `/evidence` | Record evidence |
| `POST` | `/beliefs` · `/beliefs/{id}/revisions` · `/beliefs/{id}/retract` | Belief lifecycle |
| `GET` | `/beliefs/{id}` · `/beliefs/{id}/history` | Belief with evidence and dependencies · lineage |
| `POST` | `/dependencies` | Add or re-affirm a dependency edge |
| `GET` | `/events` | Cognitive event log (scope-filtered) |
| `GET` | `/debt` | Cognitive Debt Index report |
| `POST` | `/observations` | Store raw input and queue perception (202) |
| `GET` | `/observations/{id}` | Observation, its job, and extraction runs |
| `POST` | `/observations/{id}/purge` · `/evidence/{id}/purge` | Governed purge (idempotent) |
| `POST` | `/memories/{id}/resolve` · `/beliefs/{id}/resolve` | Queue resolution explicitly (202) |
| `GET` | `/proposals` · `/proposals/{id}` | Inspect resolution proposals (`?status=pending`) |
| `POST` | `/proposals/{id}/accept` · `/reject` | Decide a proposal |
| `GET` | `/jobs` · `/jobs/{id}` | Background job state (`?status=dead_lettered`) |

Callers are identified by `x-cs-tenant`, `x-cs-user`, `x-cs-agent` and `x-cs-projects`. Set `CS_API_KEY` to also require `Authorization: Bearer <key>`. In production, put an identity-aware proxy in front of the service to set these headers. See [`crates/cs-server/src/auth.rs`](crates/cs-server/src/auth.rs).

Errors are always `{"error": {"code": "...", "message": "..."}}`.

## Workspace

| Crate | Responsibility | Depends on |
|---|---|---|
| `cs-core` | IDs, scope and access, provenance, bitemporal time, events, errors | — |
| `cs-memory` | Memory domains, beliefs, evidence, recall contracts | core |
| `cs-policy` | Deterministic recall scoring and cognitive-debt evaluation | core, memory |
| `cs-storage` | Storage contract and the in-memory reference store | core, memory |
| `cs-postgres` | PostgreSQL + pgvector adapter, migrations | core, memory, storage |
| `cs-engine` | The embeddable engine and public Rust API | core, memory, policy, storage |
| `cs-server` | HTTP transport, service binary and in-process worker | engine, core |
| `cs-extractor-http` | Optional OpenAI-compatible / Ollama extractor | engine, memory |

Core domain logic never depends on PostgreSQL, HTTP or a model provider. See [ADR-0001](docs/adr/0001-rust-workspace-and-layered-dependencies.md).

## Development

```bash
cargo test --workspace                       # unit, HTTP and in-memory conformance tests
docker compose -f deploy/compose.yaml up -d postgres
CS_TEST_DATABASE_URL=postgres://cs:cs@localhost:5432/cs cargo test -p cs-conformance
cargo clippy --workspace --all-targets -- -D warnings
```

## Documentation

- [Architecture overview](docs/architecture/overview.md) and [invariants](docs/architecture/invariants.md)
- [Architecture decision records](docs/adr/)
- Specifications: [bitemporal semantics](docs/specifications/bitemporal-semantics.md), [scope & access](docs/specifications/scope-and-access.md), [recall scoring](docs/specifications/recall-scoring.md), [cognitive debt](docs/specifications/cognitive-debt.md), [resolution](docs/specifications/resolution.md)
- [Research program](docs/research/research-program.md) and [roadmap](ROADMAP.md)

## Lineage

Cognitive Substrate continues the ideas explored in [Layered Memory Architecture](https://github.com/Kesh3805/Layered-Memory-Architecture): structured multi-tier memory, deterministic retrieval gating and background cognition. It is a clean-room rewrite with a broader research target, and no code is carried over.

## License

[Apache-2.0](LICENSE)
