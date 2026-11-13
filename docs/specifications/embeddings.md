# Embeddings — preprocessing v1

## Space id

```
provider/model@revision+d{dimensions}+{l2|raw}+pp{preprocessing}[+px{fnv32(document_prefix ␟ query_prefix)}]
```

Examples:

- `cs/hashing@1+d384+l2+pp1`: the built-in baseline.
- `ollama/nomic-embed-text@v1.5+d768+l2+pp1+px1a2b3c4d`
- `fastembed/bge-small-en-v1.5@fastembed-7+d384+l2+pp1+px…`

## What is embedded

| Record | Projection |
|---|---|
| Memory | `MemoryContent::index_text()` (domain-specific canonical text) |
| Belief | The proposition. Polarity and evidence are separate signals, never embedded. |
| Query | The query text |

The projection is capped at 8,192 characters, keeping the first 6,141 and the last 2,048 joined by ` … `. The space's document or query prefix is then prepended. `content_hash = sha256("pp1␟" + prepared_text)`.

Empty projections are rejected before any call.

## Adapter contract

Adapters return exactly one vector per input, in order. The engine then rejects any vector that:

- has the wrong dimensions;
- contains a non-finite value;
- has zero norm.

In normalized spaces, the engine L2-normalizes each vector in Rust.

## HTTP adapter

| Concern | Behaviour |
|---|---|
| Protocols | OpenAI-compatible `POST /embeddings`; Ollama native `POST /api/embed` with `truncate: false` |
| Batching | Flushed at `max_batch_items` (64) or when the total UTF-8 size would exceed `max_batch_bytes` (96,000) |
| Poison items | A batch rejected with 400, 413 or 422 is bisected, so only the offending item fails |
| Ordering | OpenAI `data` is sorted by `index`; the indices must be exactly `0..n` |
| Retryable | Transport errors, 408, 429, 5xx |
| Permanent | All other errors, and malformed responses |

## Storage

- Vectors live per `(revision, model_name, model_version)` in `memory_embeddings` and `belief_embeddings`, with `dimensions`, `content_hash` and `embedded_at`.
- Inserts are idempotent upserts that skip revisions which no longer exist.
- Recall as of *T* joins only vectors with `embedded_at <= T`.

## Scoring with partial coverage

| Situation | Effect |
|---|---|
| Query vector available | `S = w_d·D + w_l·L + w_r·R`. A candidate without a vector uses the median `D` of the candidates that have one (0.5 if none do). |
| Query vector unavailable | `S = w_l'·L + w_r'·R` with `w_l' = w_l/(w_l+w_r)` and `w_r' = w_r/(w_l+w_r)`; the trace records `dense: unavailable` |
