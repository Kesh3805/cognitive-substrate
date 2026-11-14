# Capabilities and evaluation

## Run outcome

`outcome(execution, now)`:

| Status | Verifications | Outcome |
|---|---|---|
| aborted | any | failure |
| started, `now − started_at > run_timeout` | — | failure |
| started | — | pending |
| completed | strongest tier present among `deterministic` < `independent` | failure if any verification at that tier failed, else success |
| completed | only `self_assessed`, any failed | failure |
| completed | none, or only `self_assessed` passes | unverified |

Only successes and failures are **decisive**.

## Field evidence

These rules use executions with no `case_id` and with `procedure_fingerprint` equal to the current one.

- For each input class *c* with decisive runs, `rate_c = successes_c / runs_c`.
- `n = |classes|`, `s = Σ rate_c`.
- Wilson interval with `z`: `p = s/n`. The bounds are `(p + z²/2n ± z·√(p(1−p)/n + z²/4n²)) / (1 + z²/n)`.

## Drift

Decisive field runs are ordered newest first by `(started_at, id)`.

- `consecutive_failures` = the number of leading failures.
- The window is the most recent `window` (25) decisive runs, with its Wilson upper bound.
- **Quarantine** when `consecutive_failures ≥ 3`, or when the window has at least `window_min` (10) runs and its upper bound is below `quarantine_upper_bound` (0.6).

## Suites

A case's result is the latest decisive run bound to it on the current procedure (`(started_at, id)` descending). For a `negative` case, an abort with `precondition_failed` counts as a success. A case **passes** when its result is a success.

## Eligibility

Blockers:

- a required kind (`positive`, `negative`, `boundary`) has no case;
- a required-kind or locked case is not passing;
- a quarantine signal is present.

With no blockers, the capability is eligible for:

- **trusted** if `n ≥ 30` and the lower bound is `≥ 0.8`;
- otherwise **evaluated** if `n ≥ 10` and the lower bound is `≥ 0.6`.

## Transitions

| From | To | Condition |
|---|---|---|
| observed, quarantined | candidate | none |
| candidate | evaluated | eligible ≥ evaluated |
| evaluated | trusted | eligible = trusted |
| candidate, evaluated, trusted | quarantined | manual, or automatic on a quarantine signal |
| trusted | evaluated, candidate | manual demotion |
| evaluated | candidate | manual demotion |
| any except deprecated | deprecated | none (terminal) |

## Failure ratchet

When a verification is recorded at tier `deterministic` or `independent` with verdict `failed`, on a `completed` execution with no case, a case is created **in the same transaction**: `regression`, locked, origin `ratchet { execution_id }`, with the execution's input class. If a regression case for that class already exists, it is not created again.

## API

| Method | Path | Body | Effect |
|---|---|---|---|
| GET | `/v1/capabilities/{id}` | | Memory, suite and evaluation |
| POST | `/v1/capabilities/{id}/stage` | `{ stage, reason }` | Lifecycle transition |
| POST | `/v1/capabilities/{id}/cases` | `{ kind, input_class, description? }` | Add a case (`regression` cases are locked) |
| POST, GET | `/v1/capabilities/{id}/executions` | `{ input_class, case_id? }` | Start an execution (201) · list them, newest first |
| GET | `/v1/executions/{id}` | | Execution with its verifications |
| POST | `/v1/executions/{id}/finish` | `{ status, abort_reason? }` | Finish once (`conflict` if already finished) |
| POST | `/v1/executions/{id}/verifications` | `{ tier, verdict, detail? }` | Verify a finished execution (201) |

`input_class` is 1–128 characters and opaque. `detail` and `description` are at most 1,000 characters.
