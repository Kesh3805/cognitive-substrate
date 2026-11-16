# 010 — Results (held-out seeds, PostgreSQL)

- **Run:** held-out seeds 601–605 (1,032 questions), on PostgreSQL 17.12 with pgvector 0.8.7, in a dedicated benchmark database. Metacognition version 2, detector `accounted-terms/1`.
- **Data:** manifest, raw per-question rows (including the text each answer came from) and aggregates are in [`results/held-out-postgres/`](results/held-out-postgres/).

## Verdict

**Adopted: every gate passed.** Metacognitive policy v2 is available to recalls that ask for it.

## Gates

| # | Criterion | Value | Pass |
|---|---|---|---|
| 1 | Fewer wrong answers than margin-only (CI upper bound < 0) | −0.170, CI [−0.193, −0.147] | ✅ |
| 2 | Risk among answered ≤ 0.05 | 0.000 (423 answered) | ✅ |
| 3 | Literal present questions answered ≥ 0.90 | 1.000 (423 of 423) | ✅ |
| 4 | Paraphrases answered wrongly ≤ 0.05 | 0.000 (all 129 investigated) | ✅ |
| 5 | Absent / conflict / candidate-only / qualified not answered ≥ 0.90 | 1.000 / 1.000 / 1.000 / 1.000 | ✅ |
| 6 | Exact replay reproduces every decision | 1,032 / 1,032 | ✅ |
| 7 | Zero scope violations | 0 | ✅ |

## Selective answering

| Strategy | Coverage | Risk among answered | Present questions answered |
|---|---|---|---|
| policy | 0.410 | **0.000** | 0.766 |
| margin_only (threshold 0.0546, frozen) | 0.398 | 0.426 | 0.466 |
| answer_always | 1.000 | 0.617 | 1.000 |

At almost the same coverage, margin-only answered 43% of its questions wrongly and the policy answered none wrongly. The policy also answered more of the answerable questions (77% against 47%). It did so by answering different questions, not more of them.

## Decisions by stratum

| Stratum | Questions | Answer | Investigate | Abstain | Reason |
|---|---|---|---|---|---|
| present: literal (direct, lexical trap, temporal, scope, framed) | 423 | 100% | — | — | — |
| present: paraphrase | 129 | — | 100% | — | `aspect_unmatched` |
| absent | 150 | — | — | 100% | `not_covered` |
| conflict | 150 | — | 100% | — | `contradiction` |
| candidate-only | 60 | — | 100% | — | `only_unvalidated` |
| qualified ("not", "before", "only") | 120 | — | 100% | — | `aspect_unmatched` |

## What this shows

- **Structure separates subject from aspect, and frequency could not.** Under the first detector (IDF coverage), "polar forge is owned by which team" was answered from "cobalt forge is owned by the team payments", because the framing words were as rare as the name. Requiring a memory's subject to be named in full, and every remaining question term to be accounted for, removed that leak entirely. Twins ("orion ledger" and "orion ledger replica") resolve through the most specific subject.
- **"Investigate" carries real information.**
  - **Absent:** the policy abstains when nothing is about what the question names.
  - **Aspect unmatched:** it investigates when something is, but not about what is asked. This covers every paraphrase and every qualified question.
  - **Conflict and unvalidated evidence:** it investigates when the evidence disagrees or is not yet validated.
- **Framing generalises, within its list.** Framings never used in development ("can you show me", "would you let me know", "kindly tell me currently") were all answered. Qualifiers never used in development ("not", "before", "only") were all held back.

## Limits

- **Synthetic templates.** The questions come from fixed templates whose predicates match the stored facts word for word. A real question phrased differently from its evidence is investigated, not answered: here, every paraphrase was. This result shows that the policy is **safe** on such questions, not that it is **helpful** on them.
- **Lexical only.** Paraphrases, synonyms and reverse edges ("who owns X" against "payments owns X") are not covered. A semantic detector would need its own experiment.
- **One rule set, no calibration.** The decision makes no probability claim. A margin rule exists (`min_margin`) but was off here.
- **Contradictions need structure.** Contradiction detection requires structured, single-valued facts. Two notes that disagree in prose are not detected.
