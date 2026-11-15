# 009 — Results (held-out seeds, PostgreSQL)

- **Run:** held-out seeds 501–505 (785 queries, each under 3 strategies, in rotated order), on PostgreSQL 17.12 with pgvector 0.8.7, in a dedicated benchmark database.
- **Commit:** `377c77f` ([`manifest.json`](results/held-out-postgres/manifest.json)).
- **Data:** raw per-query rows in [`raw.jsonl`](results/held-out-postgres/raw.jsonl); aggregates and paired deltas in [`summary.json`](results/held-out-postgres/summary.json).

## Verdict

**Not adopted.**

- **The p95 gate failed:** 0.959× F200 against a gate of ≤ 0.9×.
- **Every other gate passed.**

`Adaptive` stays an opt-in budget and `Fixed` stays the default. Under the preregistration, the implementation is not optimised again within this experiment.

## Gates

| # | Criterion | Value | Pass |
|---|---|---|---|
| 1 | nDCG@10 CI lower bound of A − F200 ≥ −0.01 | +0.0016, CI [−0.0014, +0.0045] | ✅ |
| 2 | Candidate recall, mean A − F200 ≥ −0.01 | +0.0000 | ✅ |
| 3 | Mean candidates scored ≤ 0.6× F200 | 0.484× (96.8 vs 200) | ✅ |
| 4 | p95 latency ≤ 0.9× F200 | **0.959×** (30.62 vs 31.95 ms) | ❌ |
| 5 | p99 latency ≤ 1.0× F200 | 0.981× (37.81 vs 38.55 ms) | ✅ |
| 6 | Exact replay reproduces selection and decision | 785 / 785 | ✅ |
| 7 | Zero scope and temporal violations | 0 | ✅ |

## Overall

| Strategy | nDCG@10 | MRR@10 | Candidate recall | Candidates | p50 ms | p95 ms | p99 ms |
|---|---|---|---|---|---|---|---|
| `A` (adaptive 50/25 → 200/100) | 0.7717 | 0.7170 | 0.9809 | 96.8 | 18.99 | 30.62 | 37.81 |
| `F200` (fixed, control) | 0.7701 | 0.7151 | 0.9809 | 200.0 | 26.41 | 31.95 | 38.55 |
| `F50` (A's first pass only) | 0.6479 | 0.6128 | 0.7892 | 50.0 | 19.13 | 24.61 | 30.60 |

Against F50, A gains:

| Metric | Gain | 95% CI |
|---|---|---|
| nDCG@10 | +0.124 | [+0.103, +0.144] |
| Candidate recall | +0.192 | [+0.164, +0.219] |

## By family

| Family | Queries | Expansion rate | Candidate recall A / F200 / F50 | nDCG@10 A / F200 / F50 | Mean candidates A |
|---|---|---|---|---|---|
| direct | 250 | 15.2% | 0.992 / 0.992 / 0.922 | 0.892 / 0.889 / 0.837 | 72.8 |
| lexical trap | 66 | 21.2% | 0.985 / 0.985 / 0.833 | 0.916 / 0.914 / 0.794 | 81.8 |
| semantic | 250 | 14.0% | 1.000 / 1.000 / 0.908 | 0.605 / 0.607 / 0.566 | 71.0 |
| temporal | 94 | 81.9% | 0.979 / 0.979 / 0.628 | 0.947 / 0.943 / 0.612 | 172.9 |
| scope | 50 | 12.0% | 0.980 / 0.980 / 0.940 | 0.980 / 0.973 / 0.940 | 68.0 |
| crowded | 75 | 100% | 0.880 / 0.880 / 0.013 | 0.439 / 0.439 / 0.013 | 200.0 |

## The expansion signal

| Measure | Value |
|---|---|
| Expansions | 245 of 785 recalls (31.2%) |
| Expansions where A's candidates held a relevant memory F50 missed | 153 (62%) |
| Recalls that did **not** expand where F200 held more relevant candidates than A | **0** |

## Latency by expansion

| Queries | A p50 / p95 / p99 ms | F200 p50 / p95 / p99 ms |
|---|---|---|
| Not expanded (540) | 18.33 / 21.14 / 23.77 | 25.79 / 30.00 / 36.56 |
| Expanded (245) | 28.20 / 36.20 / 41.69 | 27.85 / 35.37 / 56.43 |

## What this shows

- **The signal is exact on this workload.** Across 785 held-out queries it never left a relevant candidate behind that the full budget would have found. A matched F200's candidate recall exactly while scoring 48% as many candidates.
- **The first pass alone would be a real regression.** F50 loses 19 points of candidate recall: almost everything in the crowded family, and over a third of temporal targets. The expansion is doing necessary work, not decorative work.
- **The saving is real but lands in the body of the distribution, not the tail.** Non-expanded recalls are about 30% faster than F200 on the same queries, at both p50 and p95. Expanded recalls cost what F200 costs. With 31% of recalls expanding, the slowest 5% of the workload is made almost entirely of expanded recalls, so the aggregate p95 sits close to F200's.
- **The expansion rate is set by the workload.** Temporal questions expanded 82% of the time. A replaced fact, its replacement, and notes about neighbouring services that share a name word all tie in the best tier, so the truncation is genuine and expanding is correct. A workload with more such ties expands more and saves less.

## Development runs (disclosed in the preregistration)

| Run | p95 A / F200 | p99 A / F200 |
|---|---|---|
| Version 1 (expansion as a second round trip), development seeds | 1.33× | 1.15× |
| Version 2 (one store operation), development seeds | 0.908× | 0.85× |
| Version 2, held out (this run) | 0.959× | 0.981× |

Quality and decisions were identical across versions.

## Limits

- One synthetic corpus shape with one crowded family whose crowding was designed to cut the gold fact off. One machine and one store; latencies include trace writes.
- Candidate scoring is cheap at this corpus size (2,450 memories per seed). The saving from scoring fewer candidates would grow with heavier scoring, for example a learned reranker, which was not measured here.
- Only one first-pass size (50 / 25) was tested.
