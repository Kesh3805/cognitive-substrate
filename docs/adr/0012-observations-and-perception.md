# ADR-0012: Immutable observations and untrusted perception

- Status: Accepted
- Date: 2026-11-13

## Context

Raw input (messages, documents, tool output) has to become structured knowledge. Extractors, whether rule-based or model-based, can be wrong, and model output is adversarial input by default.

## Decision

- An **observation** is immutable source material: scope, content (up to 64 KiB), SHA-256 hash, source, actor and observation time. `POST /v1/observations` stores it **and** enqueues its perception job in one transaction, so an observation can never be stored and then silently left unprocessed. Idempotency keys make resubmission safe.
- Extractors implement `Extractor` (name, version, optional model). An extractor returns candidates; the engine validates and persists them. Extractors never write.
- Every extracted memory:
  - enters as **`candidate`**, whatever the extractor;
  - carries `source = observation` and `ExtractionInfo` (extractor@version, model);
  - takes its valid time from the observation's time;
  - can never be working memory.
- **One invalid candidate rejects the whole run.** No partial writes happen.
- Extraction is idempotent per `(observation, extractor, version)` through `extraction_runs`. A new extractor version produces a new run and never overwrites an earlier one.
- The built-in **rule extractor** is deterministic, offline and favours precision:
  - it scrubs code fences, block quotes, headers and URLs;
  - it vetoes questions, interrogative starts, hedges, conditionals, negations and TODOs;
  - it gates subjects (1–5 words, never pronouns or meta-nouns such as "issue" or "problem");
  - facts and relations need an **anchor token** (an identifier, acronym, version, or a Title-Case term not at the start of the sentence);
  - progressive verbs and bare evaluative adjectives are rejected;
  - decisions need an explicit marker or a stated rationale;
  - numbered steps must be strictly sequential and become one procedure.
- The optional **`cs-extractor-http`** crate talks to any OpenAI-compatible endpoint, including Ollama. It sends a versioned prompt at temperature 0 and decodes and validates strictly. Network failures, 408, 429 and 5xx are retryable; everything else is permanent. Its tests use a local mock server.

## Consequences

Perception output is reviewable before it is trusted. The extractor can be swapped without touching storage or the engine.

## Verification

`extract.rs` unit tests (including a noise corpus that must yield nothing), the `cs-extractor-http` contract tests, and the conformance scenario `perception_creates_candidates_idempotently`.
