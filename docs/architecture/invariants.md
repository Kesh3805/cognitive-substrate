# Architectural invariants

These rules are enforced in code review and, where noted, by tests.

1. **Core domain logic never depends on PostgreSQL, HTTP or a model provider.**
   Enforced by crate boundaries (ADR-0001).
2. **All authoritative mutations produce cognitive events, atomically.**
   Every mutating `CognitiveStore` method takes its event; the conformance suite checks the sequences.
3. **No model-generated content becomes trusted knowledge without an explicit validation path.**
   Model-derived memories start as `candidate` and are excluded from recall until promoted.
4. **Revisions preserve historical transaction state.**
   Superseding closes the transaction-time interval, and exclusion constraints forbid overlapping versions.
5. **Valid time and transaction time are never conflated.**
   Clients may set valid time. Only the engine assigns transaction time.
6. **Every memory and belief has provenance and scope.**
7. **Recall is policy-controlled and traceable.** Every recall persists a trace.
8. **Scope authorization is enforced before retrieval**, inside the storage query, never by filtering assembled results.
9. **Deletion accounts for derived data.** Purge removes revisions and embeddings, and event payloads never contain content.
10. **Every performance or quality claim is backed by a reproducible benchmark.**
11. **The event log is append-only at the storage boundary.** A PostgreSQL trigger rejects `UPDATE`, `DELETE` and `TRUNCATE`.
12. **Unreadable is indistinguishable from missing.** Reads of out-of-scope records return `not_found`, never `forbidden`.
13. **Detectors propose; they never mutate.** Resolution produces version-bound proposals, and only an explicit, authorized decision applies one, atomically and only if every bound revision is still current.
14. **Background effects are fenced.** A job's writes commit in the same transaction as its lease-checked completion, guarded against read skew.
15. **Extracted content is untrusted.** Every extractor's output is validated, stored only as `candidate`, and rejected wholesale if any item is invalid.
16. **Purges keep what other sources support.** Derived memories are erased with their only source, and re-homed when another source still stands behind them.
17. **Identity is never linked silently.** Names share an entity only by exact normalized key within one identity partition. Aliases and merges are explicit, version-checked decisions (ADR-0018).
18. **Merges are not retroactive.** A merge records a timestamped redirect; reads resolve identity as of their own transaction time, so earlier reads replay unchanged.
19. **A derived edge is exactly as visible as its source revision**, by scope, transaction time, status and valid time, and it is purged with it (ADR-0019).
20. **Compilation never launders evidence.** Support is counted in independent units (actor, origin, valid-time bucket) over validated, non-derived roots, with temporal dispersion and a counterexample veto. Compiled memories start as `candidate` (ADR-0020).
21. **Derived knowledge follows its support.** A purge flags citing derivations in the same transaction; losing support invalidates the derivation and demotes its memory to `candidate`.
22. **Execution status is not outcome, and self-judgment never promotes.** Success needs a deterministic or independent verification; aborts and silent runs count as failures (ADR-0021).
23. **Capability evidence cannot be inflated by repetition.** Success rates and Wilson bounds are computed over input classes, not runs; failing production runs ratchet into locked regression cases.
24. **A weak recall is not a knowledge gap.** Misses become needs only when a recall opts in, at the present time; demand and evidence availability are separate; asking establishes demand, not absence (ADR-0024).
25. **Absence is declared by people, on complete evidence.** Validation reports complete or incomplete searches; the detector does not propose gaps (experiment 008), and a gap can be confirmed only after a complete search found nothing.
26. **A smaller budget never hides what the full budget would find in the best tier.** An adaptive budget stops at its first pass only when the lexical cut falls below the best-matching tier; otherwise it returns exactly the fixed result. Its decision is in the trace and replays exactly (ADR-0025).
27. **An answer needs evidence that accounts for the question.** A recall that asks for a decision answers only from a validated memory that names the question's subject and accounts for every other question term, and only when covering evidence agrees. Otherwise it investigates or abstains, with reasons in the trace (ADR-0026).
28. **Only randomized evidence decides a causal claim, and only where it was sampled.** Passive co-occurrence and self-assessed outcomes never change a hypothesis's status; `supported` and `refuted` speak only for the contexts listed with the evaluation, and every evaluation is kept (ADR-0027).
29. **A counterfactual is an interpretation, never a record.** It is computed on read from randomized evidence under a supported hypothesis, labelled inferred with its assumptions, and never stored, evented or accepted as evidence (ADR-0028).
