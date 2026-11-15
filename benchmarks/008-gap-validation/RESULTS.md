# 008 — Results (held-out seeds, PostgreSQL)

- Run: held-out seeds 401–405 (about 2,300 validated questions); store: PostgreSQL; detector `idf-coverage/1`; signature v1.
- Commit: `f867e49` ([`manifest.json`](results/held-out-postgres/manifest.json)). Raw rows: [`raw.jsonl`](results/held-out-postgres/raw.jsonl). Aggregates and gates: [`summary.json`](results/held-out-postgres/summary.json).

## Verdict

**Tier A (automatic validation) and Tier B (gap proposals) both fail. Under the preregistered rule, validation stops proposing gaps.**

- A complete search that finds nothing is recorded on the need, but its status does not change.
- Evidence still satisfies a need, or marks it pending promotion.
- A person may confirm a gap after a complete, empty validation.
- Gap proposals return only with a detector that passes a new preregistered experiment.

## Per stratum

| Stratum | n | Error rate | 95% upper | Outcome counts |
|---|---|---|---|---|
| present_direct | 400 | **0.540** (false gaps) | 0.588 | candidate_gap 216, satisfied 184 |
| present_paraphrased | 400 | **0.938** (false gaps) | 0.957 | candidate_gap 375, satisfied 25 |
| present_unvalidated | 363 | 0.000 | 0.011 | pending_promotion 363 |
| absent_standard | 397 | 0.000 (missed gaps) | 0.010 | candidate_gap 397 |
| absent_boilerplate | 369 | 0.000 | 0.010 | candidate_gap 369 |
| absent_other_scope | 359 | 0.000 | 0.011 | candidate_gap 359 |

Leaked evidence: 0. Unvalidated-only evidence reported as satisfied: 0. Incomplete validations: 0.

Breakdown of `present_direct`, descriptive only:

| Half | False gaps |
|---|---|
| concise | 16 of 200 (8%) |
| verbose | 200 of 200 (100%) |

## Gates

| Gate | Tier A | Tier B |
|---|---|---|
| present_direct false gaps | ❌ 0.540 vs ≤ 0.01 | ❌ 0.540 vs ≤ 0.05 |
| present_paraphrased false gaps | ❌ 0.938 vs ≤ 0.10 | — |
| present_unvalidated false gaps | ✅ 0.000 (upper 0.011) | — |
| absent_standard missed gaps | ✅ 0.000 (upper 0.010) | ✅ |
| absent_boilerplate missed gaps | ✅ 0.000 (upper 0.010) | ✅ |
| absent_other_scope missed gaps | ✅ 0 | — |
| leakage / silently satisfied | ✅ 0 / 0 | ✅ |

## What this shows

- **The detector is reliable in one direction only.** It never claimed evidence for something absent: 0 of 1,125 absent questions, including boilerplate questions and facts held only by another user. It also never leaked another scope's evidence. But it often claims absence for knowledge that is present, which is the more damaging error.
- **Verbose questions defeat a lexical detector.** Filler words that occur nowhere in the substrate look exactly like a missing entity, so every verbose question about a present fact became a false gap. Even concise questions in the fact's own words fail 8% of the time, above the 5% proposal limit.
- **Paraphrases fail as expected.** 94% false gaps. Lexical coverage cannot recognize different wording, and the hashing embedder adds nothing.
- **Unvalidated knowledge is handled correctly.** All 363 questions answered only by candidate memories ended in `pending_promotion`, never `satisfied`, so the need points at memories waiting for review instead of being closed.

## What it would take to try again

A detector that understands meaning rather than shared words. For example, a calibrated dense-similarity rule with a learned embedder, or entity-anchored matching (M4.1 identity) combined with relation matching. It would need its own preregistration, with the same strata.
