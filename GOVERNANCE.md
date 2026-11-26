# Governance

## Current model

Cognitive Substrate has a single maintainer, [Kesh3805](https://github.com/Kesh3805), who has final say on what is merged and released (a "benevolent dictator" model). This reflects the project's size today, not a preference; the path below describes how it changes.

## How decisions are made

Decisions are made in the open and written down:

- **Architecture.** Any change that adds a capability, changes a storage semantic or breaks an [invariant](docs/architecture/invariants.md) needs an ADR in [`docs/adr/`](docs/adr/). An ADR is proposed in a pull request, discussed there, and merged as `Accepted` (or closed). Superseded ADRs are marked, never deleted.
- **Quality and performance claims.** These are settled by preregistered experiments under [`benchmarks/`](benchmarks/): the hypothesis, metrics and decision rule are committed before results are produced, and the results are reported whether or not they favour the change. A feature that fails its decision rule is not adopted as a default.
- **Everything else** (bug fixes, documentation, tests) is decided in pull-request review.

Anyone may propose an ADR or an experiment. The maintainer decides, and records the reasoning in the ADR or the pull request.

## Becoming a maintainer

The project wants more maintainers. A contributor who has made sustained, high-quality contributions (code, review, or experiments) over several months, and who understands the invariants, may be invited to become a maintainer by the existing maintainers. Once there are three or more maintainers, decisions that cannot be settled in review move to a simple majority of maintainers, and this document will be updated to say so.

## Continuity

The project currently has a bus factor of one. To limit the damage if the maintainer becomes unavailable:

- all design reasoning lives in the repository (ADRs, specifications, experiment reports), not in private channels;
- the code is Apache-2.0, so anyone may fork and continue it;
- the goal is to give a second trusted person repository administration rights once one is identified; they will be named here.

## Contributions and sign-off

The project uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO), not a contributor license agreement (CLA). Every commit in a pull request must be signed off (`git commit -s`), certifying that you have the right to submit it under the project's license. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Code of conduct and security

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). Security issues follow the [security policy](SECURITY.md).
