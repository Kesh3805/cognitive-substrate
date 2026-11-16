# 010 — Answer, investigate or abstain

- Status: **preregistered** (committed before any run on held-out seeds 601–605)
- Date: 2026-11-16
- Concerns: M6.1, metacognitive policy v2 with coverage detector `accounted-terms/1`.

## Question

Can a deterministic policy over a recall's own selection decide when to **answer**, when to **investigate** and when to **abstain**? It should give far fewer wrong answers than answering always or answering by score margin, while still answering the questions its evidence states literally.

## Policy (frozen: metacognition version 2)

The recall runs with `channels { lexical: 100, dense: 0 }`, `max_candidates: 200`, `max_results: 10`, candidates included, and `metacognition: { min_margin: null }`.

**Coverage detector `accounted-terms/1`.** The question's terms are the shared lexical contract's terms, compared up to one trailing `s`. Conversational framing terms (`can could currently kindly know let me now please show tell would you`) are ignored. Negations and temporal qualifiers are not.

| Memory | Covers the question when | Anchored when |
|---|---|---|
| **Structured** (subject, predicate and object) | All of its subject terms appear in the question, and every question term appears in its subject, predicate or object | Its subject terms appear in the question but some question term is unaccounted for |
| **Unstructured** | It contains every question term | Never |

When several memories cover a question, the answer comes from the one with the most subject terms, then the highest score, then the best rank.

**Rules, in order:**

| Situation | Decision |
|---|---|
| Nothing selected | Abstain |
| Nothing covering, nothing anchored | Abstain |
| Nothing covering, something anchored (`aspect_unmatched`) | Investigate |
| Only unvalidated memories cover | Investigate |
| Covering validated memories state different values for the same subject and predicate, valid at the same time | Investigate |
| Otherwise | Answer |

Each decision records its reasons, its signals and the terms it accounted for.

## Data

The data starts from 005's seeded generator. Gold labels come from the generator, never from text similarity. Dependencies are stored as relational edges, because they are many-valued. Strata and families:

| Stratum | Family | Built how | Right action |
|---|---|---|---|
| present | direct, lexical trap, temporal, scope | 005's questions, about services not chosen below | answer from a gold memory |
| present | semantic (paraphrases) | 005's paraphrase questions | answer correctly or not at all |
| present | framed | a direct question behind conversational framing | answer from a gold memory |
| absent | — | every fact about a service is removed, then a direct question is asked about it | not answer |
| conflict | — | a second, different store fact is added for a service, both valid now | not answer |
| candidate-only | — | a service's only store fact is unvalidated | not answer |
| qualified | — | a direct question with a qualifier appended | not answer |

The held-out seeds change the mix and the wording:

| | Development (1, 2, 3) | Held out (601–605) |
|---|---|---|
| absent / conflict / candidate-only | 20 / 20 / 20 | 30 / 30 / 12 |
| framed / qualified | 25 / 25 | 24 / 24 |
| framings | "please tell me", "could you tell me" | "can you show me", "would you let me know", "kindly tell me currently" |
| qualifiers | "previously", "never" | "not", "before", "only" |

The store is PostgreSQL + pgvector, in a dedicated benchmark database, with a fresh tenant per seed.

## Strategies compared on the same recall

- **policy:** the decision above. When it answers, it answers from its first covering memory.
- **answer_always:** answers from the top selected memory.
- **margin_only:** answers from the top selected memory when it leads the next by at least **0.0546**, and otherwise abstains. The threshold was frozen from development seeds as the value that matches the policy's coverage.

An answer is an **error** unless the question is present and the memory answered from is a gold memory. Not answering is never an error, and is reported as coverage.

## Adoption criteria (all must hold on held-out seeds)

1. **Fewer wrong answers than margin-only:** the paired per-question delta in "answered and wrong", policy − margin_only, has a 95% CI upper bound < 0 (10,000 bootstrap resamples, seed 7).
2. **Risk among answered** ≤ 0.05.
3. **Literal present questions answered** ≥ 0.90. These are the direct, lexical-trap, temporal, scope and framed families.
4. **Paraphrases answered wrongly** ≤ 0.05, of all paraphrase questions. The rate investigated is reported.
5. Not answered ≥ 0.90 for each of **absent**, **conflict**, **candidate-only** and **qualified**.
6. **Exact replay** reproduces every decision (100%).
7. **Zero scope violations.**

## Disclosure: what development seeds changed

The detector, the data and the gates were revised on development seeds 1–3 before this preregistration.

| Run | Coverage detector | Result on development seeds |
|---|---|---|
| 1 | `idf-coverage/1` (experiment 008's detector) | Conflict and candidate-only were never answered. But absent services leaked 22%, because framing words such as "owned" and "team" are as rare as name words, so IDF cannot separate subject from aspect. "depend" never matched "depends". Paraphrases were 15% answered and 69% wrong when answered. |
| 2 | `accounted-terms/1` | No wrong answers. Every literal question was answered, and every paraphrase was investigated. |

Changes between the runs:

- **Detector replaced by `accounted-terms/1`.** It was reviewed externally before freezing. The review added the enumerated framing terms, the rule that qualifiers are never ignored, the subject-collision and adversarial-framing tests, and the explanation fields in the trace.
- **Data:**
  - dependencies are stored as relational edges, so two dependencies of one service no longer look like a contradiction;
  - the policy is scored on the memory it answers from;
  - the **framed** and **qualified** families were added, with held-out wording never used on development seeds, so that the generator's exact templates cannot flatter the detector.
- **Gates:** placeholder gates written before the first run were replaced, as reviewed.
  - "present answered ≥ 0.80" became gates 3 and 4. It rewarded answering paraphrases the detector cannot verify.
  - The absent, conflict, candidate-only, risk, replay and scope gates are unchanged, and the qualified gate was added.

Development numbers for the frozen design, identical on both stores:

| Strategy | Coverage | Risk among answered |
|---|---|---|
| policy | 0.4625 | 0.000 |
| margin_only | 0.4609 | 0.424 |
| answer_always | 1.0 | 0.575 |

Every gate passed on development seeds. **These numbers are not evidence.** Only the held-out seeds decide. Because the templates are synthetic, a held-out pass shows the policy behaves as specified on wording it was not tuned to. It does not show that it handles natural language in general.

## If adopted

- Metacognitive policy v2 becomes available to recalls that ask for it (`metacognition` in the recall policy). It is never on by default, because it adds a decision, not a ranking.
- The ADR records what the detector cannot do: paraphrases and reverse edges are investigated, not answered.

If any criterion fails, the failure is reported and the policy stays experimental. Criteria are not changed after results exist.
