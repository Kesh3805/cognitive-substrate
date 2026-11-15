# ADR-0024: Information needs, and gaps only by evidence

- Status: Accepted
- Date: 2026-11-15

## Context

"The system could not answer" is easy to record and easy to over-interpret. A weak recall may come from bad phrasing, a ranking failure, a missing channel or genuinely missing knowledge. Treating every weak recall as a knowledge gap would drive acquisition toward knowledge that already exists. That creates duplicates and, eventually, contradictions.

## Decision

### Three states, not one

- **Retrieval failure:** one recall, with a lexical candidate channel and no unavailable dense channel, found no candidate carrying at least half the query's IDF weight (detector `idf-coverage/1`).
- **Unresolved need:** the same need recurred in a second distinct session, or was asked explicitly. Asking establishes **demand**, never absence.
- **Gap:** absence of evidence, decided as described below.

**Demand and availability are separate fields.**
- *Demand* is sessions and occurrences. One session counts once, and without a session key one UTC day is one session.
- *Availability* is the status, validation outcome and evidence references.

Only availability decides whether something is a gap.

### Recording is opt-in, present-time and private

- **Opt-in:** recall records needs only when its policy sets `record_information_needs`, so reads never write by default.
- **Present-time only:** historical (as-of) recalls neither record nor satisfy needs, because they legitimately miss what was learned later.
- **Private:** needs belong to the caller's own scope.
- **No query text:** observed needs keep a versioned **lexical** signature (distinct terms) and a hash, not the query text. Signatures can still reveal intent, so needs can be purged directly.
- **Evidence satisfies:** a later recall that finds qualifying evidence marks the need satisfied, with exact revision references.

### Validation is complete or says so

`validate_need` searches every eligible revision, active or candidate, that shares a term with the need, as of now. Term statistics cover exactly that population. The outcome is one of:

| Outcome | Effect |
|---|---|
| **Complete, evidence found** | The need is satisfied, or `pending_promotion` if all the evidence is unvalidated, in which case promoting or rejecting those memories resolves it |
| **Complete, no evidence** | Recorded on the need |
| **Incomplete** (the budget would be exceeded) | Never treated as absence |

### Gaps are a person's call, by evidence

Experiment 008 ([results](../../benchmarks/008-gap-validation/RESULTS.md)) measured the detector on held-out seeds:

- **Never wrong about absence:** 0 missed gaps across 1,125 absent questions, and zero leaked evidence.
- **Often wrong about presence:**
  - 54% of direct questions about present facts were declared gaps: every verbose question, and 8% of concise ones;
  - 94% of paraphrased questions were.

Both the automatic tier and the proposal tier failed. **Validation therefore does not propose gaps.** A person may mark a need as a gap (`confirm_gap`) only after a complete validation found nothing. The `candidate_gap` status stays in the model for a future detector that passes a new experiment.

### Acquisition priority is a heuristic, named as one

```
P = ln(1 + sessions) × 0.5^(age_days / 14) × status weight × impact / cost
```

- Impact and cost are each in 1..=5.
- Every component is returned with the result.
- It is not called information gain. That needs calibrated priors over outcomes, which the system does not have yet.

## Consequences

- The system can tell callers what it repeatedly could not answer, and can show which needs have evidence waiting in unvalidated memories. It cannot yet say on its own that knowledge is missing.
- Phrasing variants of one need stay separate needs (signatures are exact). Merging them by rare terms would make identity drift as the corpus changes.
- A semantic detector, such as calibrated dense similarity with a learned embedder or entity-anchored matching, is the path to gap proposals, under a new preregistration with 008's strata.
