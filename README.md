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

### Embeddings & operations (M3.x)

| Capability | What it means |
|---|---|
| **Pluggable embedders** | The built-in deterministic baseline, any OpenAI-compatible endpoint, Ollama's native API, or local ONNX models via FastEmbed (optional build). |
| **Embedding spaces** | Each vector is tied to an immutable space: model, revision, dimensions, normalization, preprocessing and query/document prefixes. Vectors from different spaces are never compared. |
| **Embedder-independent writes** | Writes succeed during provider outages. Backfill jobs commit atomically with the write, and backfilled vectors re-trigger dense resolution. |
| **Honest degradation** | If a query cannot be embedded, recall falls back explicitly to lexical + recency and records why. Candidates still awaiting a vector get a neutral score. |
| **Replay stays exact** | Traces store the query vector, and recall as of *T* only uses vectors that existed at *T*, so backfills never change a replay. |
| **Zero-downtime model switch** | Dual-write a shadow space, backfill newest first, watch coverage, then cut over. Startup refuses to serve an under-backfilled active space. |
| **Process roles** | `api` nodes never claim jobs and `worker` processes serve no HTTP; `all` is for development. |

### Entities & graph (M4.1–M4.2)

| Capability | What it means |
|---|---|
| **Source-linked graph** | Every revision's structured fields are projected into mentions and typed edges. Each edge names the exact revision it came from and is visible only when that revision is: same scope, current as of the query, valid at the query time. |
| **Governed identity** | Names link only by exact normalized key within a read-scope partition (`AuthService` = `auth-service`, but `C++` ≠ `C#`). Aliases and kinds are explicit, version-checked decisions. Merges are proposals bound to entity versions. |
| **Non-retroactive merges** | Accepting a merge records a timestamped redirect instead of rewriting edges, so as-of reads before the merge still see two entities. |
| **Bounded traversal** | Breadth-first neighborhoods with depth, node, edge and path budgets, deterministic ordering and explicit truncation. Every reached entity comes with the path and edges that justify it. |
| **Erasure reaches identity** | Purging a memory removes its edges and any uncurated entity it alone supported. Entity names never enter the event log. |

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

To embed with a local Ollama model:

```bash
CS_EMBEDDER=ollama CS_EMBEDDER_URL=http://localhost:11434 CS_EMBEDDER_MODEL=nomic-embed-text \
CS_EMBEDDER_DIMENSIONS=768 CS_EMBEDDER_DOCUMENT_PREFIX='search_document: ' \
CS_EMBEDDER_QUERY_PREFIX='search_query: ' cargo run -p cs-server
```

Switching models on existing data? See the migration procedure in [ADR-0017](docs/adr/0017-model-migration-and-process-roles.md).

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
| `GET` | `/processing/status` | Embedding coverage per space and queue counts |
| `POST` | `/embeddings/reindex` | Backfill a space (`{"space": ...}`, default: the shadow space) (202) |
| `GET` | `/entities?name=` · `/entities/{id}` | Find entities by name · entity with aliases and mentions |
| `POST` | `/entities/{id}/aliases` · `/entities/{id}/kind` | Version-checked alias or kind |
| `POST` | `/entities/{id}/merge-candidates` · `/entity-merges` | Detected or explicit merge proposals |
| `POST` | `/entities/{id}/neighborhood` | Bounded, as-of graph traversal with paths |

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
| `cs-embedder-http` | OpenAI-compatible and Ollama-native embedder | engine |
| `cs-embedder-fastembed` | Local ONNX embedder (not built by default; server feature `fastembed`) | engine |

Core domain logic never depends on PostgreSQL, HTTP or a model provider. See [ADR-0001](docs/adr/0001-rust-workspace-and-layered-dependencies.md).

## Development

```bash
cargo test                                   # unit, HTTP and in-memory conformance tests
docker compose -f deploy/compose.yaml up -d postgres
CS_TEST_DATABASE_URL=postgres://cs:cs@localhost:5432/cs cargo test -p cs-conformance
cargo clippy --all-targets -- -D warnings
cargo test -p cs-embedder-fastembed          # optional ONNX embedder (CS_TEST_FASTEMBED=1 runs a real model)
```

## Documentation

- [Architecture overview](docs/architecture/overview.md) and [invariants](docs/architecture/invariants.md)
- [Architecture decision records](docs/adr/)
- Specifications: [bitemporal semantics](docs/specifications/bitemporal-semantics.md), [scope & access](docs/specifications/scope-and-access.md), [recall scoring](docs/specifications/recall-scoring.md), [cognitive debt](docs/specifications/cognitive-debt.md), [resolution](docs/specifications/resolution.md), [embeddings](docs/specifications/embeddings.md), [entities & graph](docs/specifications/entities-and-graph.md)
- [Research program](docs/research/research-program.md) and [roadmap](ROADMAP.md)

## Lineage

Cognitive Substrate continues the ideas explored in [Layered Memory Architecture](https://github.com/Kesh3805/Layered-Memory-Architecture): structured multi-tier memory, deterministic retrieval gating and background cognition. It is a clean-room rewrite with a broader research target, and no code is carried over.

## License

[Apache-2.0](LICENSE)
