# ADR-0005: Provenance, evidence and epistemic confidence

- Status: Accepted
- Date: 2026-11-13

## Context

A substrate that cannot say *why* it believes something cannot be corrected. Model-derived knowledge is a particular risk, because false memories propagate.

## Decision

- Every memory has a `Provenance`: source (user input, agent output, tool output, external document, human annotation or system derivation), actor, `observed_at`, `recorded_at`, optional extraction info (model, versions) and parent evidence. `observed_at` and `recorded_at` are never substituted for valid or transaction time.
- Model-derived content enters as **`candidate`** and is excluded from recall until it is explicitly promoted (or rejected) with a reason.
- **Beliefs are separate from memories.** A belief has a proposition, a polarity, a `Confidence { value ∈ [0,1], interpretation, method }`, and evidence links (`supports`, `refutes` or `contextual`, with optional strength).
- A confidence must declare what it means: calibrated probability, heuristic score, human assessment or model estimate. A heuristic is never presented as a probability.
- Refuting evidence makes a belief `disputed` unless the revision explicitly sets a status. Conflicting evidence is never averaged away.
- Evidence is a normalized, tenant-scoped record. Beliefs cannot cite another tenant's evidence.

## Consequences

Memory provenance is JSONB on the record, while belief evidence is normalized. M3 may unify the two if query requirements justify it.

## Verification

The `model_derived_memory_requires_validation` and `refuting_evidence_disputes` conformance scenarios.
