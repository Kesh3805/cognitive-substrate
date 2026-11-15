# 008 — How far can gap validation be trusted?

- Status: **preregistered** (committed before any run on held-out seeds 401–405)
- Date: 2026-11-15

## Question

`validate_need` searches a caller's whole eligible substrate and decides whether a need has evidence, using detector `idf-coverage/1`. How often is that wrong?

- **False gaps**: knowledge is present, but the detector says it is absent. This error is the more damaging one. It triggers unnecessary acquisition, duplicate knowledge and eventually contradictory records.
- **Missed gaps**: knowledge is absent, but the detector says evidence exists.

The answer decides:

- whether validation may mark gaps **automatically**;
- whether it may only **propose** candidate gaps for a person to confirm (the current behaviour);
- or whether it must stop proposing gaps at all.

## Setup (fixed)

### Corpus

Per seed, the corpus is:

- the experiment-005 corpus (2,000 memories, `crates/cs-bench` generator);
- 200 **framing notes** in Alice's scope ("the current deployment status of the ⟨existing service⟩ is green");
- 80 **Bob-only facts** and 80 **unvalidated facts**: Alice's candidate memories, created with extraction provenance and never promoted.

The Bob-only and unvalidated facts are about services whose names come from vocabularies that occur nowhere else. Every synthetic stratum has its own disjoint vocabulary.

### Questions

Each question is asked by Alice (`ask`), then validated once (`validate_need`). There are up to 80 per stratum per seed. Questions that collapse into one signature count once.

| Stratum | Question | Correct outcome | Error |
|---|---|---|---|
| present_direct | About one of Alice's current facts, in the fact's words. Half are made verbose by wrapping in ≈10 filler words | satisfied | false gap |
| present_paraphrased | About a current fact, in different words (the 005 semantic templates) | satisfied | false gap |
| present_unvalidated | About a fact Alice has only as an unvalidated memory | pending_promotion | false gap, or satisfied (silently satisfied) |
| absent_standard | About a service that exists nowhere. Half are verbose | candidate gap | missed gap |
| absent_boilerplate | "What is the current deployment status of ⟨service that exists nowhere⟩" | candidate gap | missed gap |
| absent_other_scope | About a fact only Bob has | candidate gap | missed gap, or leaked evidence |

Validation runs with the default engine (`auto_validate_gaps = false`), so a complete-and-empty search yields `candidate_gap`.

### Seeds and store

| Use | Seeds |
|---|---|
| Development | 1 |
| **Held-out** | 401, 402, 403, 404, 405 |

The held-out run uses PostgreSQL, a fresh tenant per seed, and the `cs/hashing@1` embedder. The embedder is irrelevant here, because detector v1 is lexical.

### Disclosure: what the development seed showed

The development seed was used to debug the pipeline, and found two bugs, both fixed before this preregistration was committed:

1. **Vocabulary collisions between strata.** The generator drew absent-question names from the same vocabulary as the unvalidated and Bob-only facts, so "absent" questions matched Alice's candidate memories. Each stratum now has its own vocabulary.
2. **A population mismatch in validation (product bug).** Validation searched active and candidate memories but computed term statistics over active ones only, so terms that occur only in candidate memories were over-weighted. Statistics now cover exactly the searched population.

After the fixes, the development seed showed:

| Stratum | Error |
|---|---|
| present_direct, concise half | 0 of 40 false gaps |
| present_direct, verbose half | **40 of 40** false gaps |
| present_paraphrased | about 89% false gaps |
| present_unvalidated | 0 errors |
| all absent strata | 0 missed gaps |

**Why verbose questions fail:** filler words that occur nowhere in the corpus get the same weight as a missing entity. A lexical detector cannot tell them apart. This is a property of detector v1 and is not tuned here, so the verbose half of `present_direct` is expected to fail on held-out seeds too.

## Metrics

- Per stratum: the error rate (false-gap rate for present strata, missed-gap rate for absent strata), with a 95% Wilson upper bound.
- Counts of incomplete validations, silently satisfied needs and leaked evidence references.
- Results are also broken down by verbose versus concise within `present_direct` and `absent_standard`. That breakdown is descriptive, not gated.

## Gates

### Tier A — automatic validation permitted

All of these must hold:

- false-gap rate, present_direct: ≤ 1% (upper ≤ 2%);
- false-gap rate, present_paraphrased: ≤ 10% (upper ≤ 15%);
- false-gap rate, present_unvalidated: ≤ 1% (upper ≤ 2%);
- missed-gap rate, absent_standard: ≤ 3% (upper ≤ 5%);
- missed-gap rate, absent_boilerplate: ≤ 5% (upper ≤ 8%);
- missed-gap rate, absent_other_scope: 0;
- zero leaked evidence, and zero unvalidated-only evidence reported as satisfied.

### Tier B — gap proposals permitted

All of these must hold:

- false-gap rate, present_direct: ≤ 5%;
- missed-gap rate, absent_standard: ≤ 8%;
- missed-gap rate, absent_boilerplate: ≤ 10%;
- zero leaked evidence, and zero silently satisfied needs.

## Decision rule

| Outcome | Consequence |
|---|---|
| **Tier A passes** | `auto_validate_gaps` may be enabled by an operator; it stays off by default. |
| **Tier B passes, Tier A fails** | Validation keeps proposing `candidate_gap` for a person to confirm (current behaviour). |
| **Tier B fails** | Validation **stops proposing gaps**. A complete search that finds nothing is recorded on the need, but its status does not change. Evidence still satisfies a need, or marks it pending promotion. Gap proposals return only with a detector that passes a new preregistered experiment. |
