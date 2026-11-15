# Information needs — signature v1, detector `idf-coverage/1`

## Signature

`Signature::of(text)` returns the distinct terms of `text` under the shared lexical contract (tokenizer v1), sorted. Its key is `v1:` followed by the terms joined by spaces. There is no signature when the text has no terms.

## Evidence (detector `idf-coverage/1`)

For a signature with terms `t₁…tₙ`, over an eligible population of `N` revisions where term `tᵢ` occurs in `dfᵢ` of them:

- `wᵢ = ln((N + 1) / (dfᵢ + 1))`, clamped at ≥ 0. If every `wᵢ` is 0, every `wᵢ` is 1 instead.
- A revision with terms `T` **qualifies** when:
  - it matches at least `min(n, 2)` of the terms (and at least 1);
  - `Σ wᵢ` over matched terms ≥ 0.5 × `Σ wᵢ`;
  - `Σ wᵢ` > 0.

The population for the statistics is exactly the population being searched:

- **for a recall**, active memories, plus candidates when the recall included them;
- **for validation**, active and candidate memories.

## Recording from recall

A recall records or satisfies a need only when **all** of these hold:

1. `policy.record_information_needs` is set;
2. the request set no as-of times;
3. the caller has a user or an agent, which gives the need's own scope;
4. the policy's candidate generation includes a lexical channel;
5. the dense channel was not `unavailable`.

Then:

| Recall result | Effect |
|---|---|
| **No candidate qualifies** (a miss) | `observe_need`: insert a need (`retrieval_failure`, one session, one occurrence), or, under a row lock, add an occurrence, count the session once, and apply `InformationNeed::observe` |
| **Some candidate qualifies** | If an open need with this signature exists, it becomes `satisfied`, with up to 10 qualifying revisions as `resolved_by` |

Under `InformationNeed::observe`, a need moves to `unresolved` in its second distinct session. A satisfied need that is missed again reopens. The session key is the request's `session` (1–128 characters), else `day:YYYY-MM-DD` (UTC).

## Explicit questions

`ask` records an `unresolved`, `explicit` need with the question text. If the signature already exists, the existing need is returned.

## Validation

`validate_need` (as of now) asks the store for up to 5,001 eligible revisions in lexical order, active and candidate:

| What it finds | Outcome | Status |
|---|---|---|
| More than 5,000 | `incomplete` | unchanged |
| Qualifying evidence, some of it active | `complete_evidence_found` | `satisfied` |
| Qualifying evidence, all of it candidate | `complete_evidence_found` | `pending_promotion` |
| No qualifying evidence | `complete_no_evidence` | **unchanged** (experiment 008) |

`confirm_gap` sets `validated_gap` on an open need whose latest validation was `complete_no_evidence`.

## Acquisition priority v1

```
sessions = ln(1 + sessions)
recency  = 0.5 ^ (age_days(last_seen) / 14)
P        = sessions × recency × weight × impact / cost
```

| Status | Weight |
|---|---|
| `validated_gap` | 1.0 |
| `candidate_gap` | 0.75 |
| `unresolved` | 0.5 |
| `pending_promotion`, `retrieval_failure` | 0.25 |
| `satisfied`, `dismissed` | 0 |

`list_needs` sorts by `P` descending, then by id.

## API

| Method | Path | Effect |
|---|---|---|
| GET | `/v1/information-needs?status=&limit=` | Needs with priority components, by priority |
| POST | `/v1/information-needs` | `{ question, impact?, cost? }`: ask (201) |
| GET | `/v1/information-needs/{id}` | One need with its priority |
| POST | `/v1/information-needs/{id}/validate` | Complete search, as above |
| POST | `/v1/information-needs/{id}/confirm` | `{ reason }`: confirm a gap after an empty, complete search |
| POST | `/v1/information-needs/{id}/dismiss` | `{ reason }` |
| POST | `/v1/information-needs/{id}/weights` | `{ impact, cost, expected_version }` |
| POST | `/v1/information-needs/{id}/purge` | Erase the need (204) |

Recall requests take an optional `session`. A recall that recorded or satisfied a need returns `information_need: { need_id, status, missed }`.

## Events

| Event | Payload |
|---|---|
| `InformationNeedObserved` | ids and counts |
| `InformationNeedValidated` | status, detector, revisions examined |
| `InformationNeedChanged` | asked, satisfied, confirmed, dismissed, re-weighted or purged |

No payload contains query text or terms.
