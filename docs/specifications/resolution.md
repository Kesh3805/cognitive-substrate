# Resolution — detector version `resolve-v1`

## Eligibility

Two records are compared only if all of the following hold:

- both are current revisions with status `active` or `candidate`;
- they are in the same domain;
- they have **identical visibility**: same tenant, project, owner user, agent and visibility level (the conversation may differ).

## Precedence (first match wins)

| # | Condition | Kind | Operation |
|---|---|---|---|
| 1 | Normalized `index_text` equal, and valid times overlap | Duplicate | `merge` into the older record |
| 2 | Structured triples equal, and valid times overlap | Duplicate | `merge` |
| 3a | Same subject and predicate, different value; single-valued domain (semantic, temporal); the later fact starts later in valid time | Supersession | `supersede`: the older fact's valid time ends where the newer one starts |
| 3b | Same as 3a, but the valid times start together and overlap | Contradiction | `quarantine`: both facts return to `candidate` |
| 4 | Cosine ≥ τ(model, domain), valid times overlap, and structural pre-veto passes | Duplicate | `merge` |
| B | Beliefs: same normalized proposition, opposite polarity, overlapping valid time | Contradiction | `dispute` both |

Normalization lowercases text and strips punctuation. Triples come from:

- semantic memories: `subject`/`predicate`/`object`, when all three are present;
- relational memories: `subject`/`predicate`/`object`;
- temporal memories: `subject`/`predicate`/`value`.

Relational predicates are multi-valued, so step 3 never applies to them.

## Structural pre-veto

`hard_tokens(text)` is the **ordered** sequence of tokens that:

- contain a digit (numbers, units, versions);
- are negations (`not`, `no`, `never`, `none`, `without`, `disabled`, `cannot`, `*n't`);
- look like identifiers (snake_case, kebab-case, paths, `::`, camelCase or PascalCase, acronyms).

Two texts may become a dense duplicate only if these sequences are identical. Because the comparison is ordered, swapped arguments (`a calls b` / `b calls a`) are vetoed too.

## Thresholds

Thresholds are keyed by `model@version` and then by domain. A missing entry disables dense duplicate detection.

| Model | Episodic | Semantic | Procedural |
|---|---|---|---|
| `cs-hashing@1` | 0.88 | 0.88 | 0.90 |

### Calibrating a new embedder

1. Build a perturbation suite per domain, with 30–50 pairs in each class:
   - **true duplicates:** paraphrases, reorderings, formatting changes;
   - **hard negatives:** argument inversion, identifier swaps, numeric and version flips, negation flips;
   - **same-topic unrelated** pairs.
2. Measure the cosine of every hard negative that **passes** the structural pre-veto, and set `τ_min = max + 0.015`.
3. Choose `τ = max(τ_min, argmax F_0.25)`. This weights precision 16× over recall, because a false merge loses information while a missed duplicate only leaves two records.
4. Encode the suite as a test asserting that no hard negative produces a duplicate (see `crates/cs-engine/tests/resolution_calibration.rs`).

For the hashing embedder, the highest non-vetoed hard negative scores 0.815 and a close paraphrase scores about 0.89, which gives the thresholds above.

## Lifecycle

```
pending ──accept (all bound revisions current)──► accepted
   │──accept (any target changed)──────────────► stale
   └──reject──────────────────────────────────► rejected
```

- Fingerprint = `kind | sorted(record@revision)`. It is unique across all statuses.
- Detection only ever inserts `pending` proposals. Nothing but an explicit decision changes a target.
