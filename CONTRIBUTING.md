# Contributing

Thanks for your interest in Cognitive Substrate.

## Ground rules

- Read the [architecture overview](docs/architecture/overview.md) and the [invariants](docs/architecture/invariants.md) first. Changes that break an invariant need an ADR.
- **Architectural decisions** go in `docs/adr/` as numbered ADRs, each with Context, Decision, Alternatives, Consequences and Verification sections.
- **Storage semantics** are defined by the in-memory reference store and the conformance suite (`tests/conformance`). A new behaviour needs a scenario there, and every adapter must pass it.
- **Quality or performance claims** need a reproducible benchmark under `benchmarks/`, preregistered before results are produced.
- Keep `cs-core`, `cs-memory` and `cs-policy` free of I/O, databases, HTTP and model SDKs.

## Workflow

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
docker compose -f deploy/compose.yaml up -d postgres
CS_TEST_DATABASE_URL=postgres://cs:cs@localhost:5432/cs cargo test -p cs-conformance
```

Commits should be focused and described in the imperative mood ("Add belief retraction endpoint").

## Sign-off (DCO)

The project uses the [Developer Certificate of Origin](https://developercertificate.org/) instead of a CLA. Sign off every commit in a pull request:

```bash
git commit -s -m "Add belief retraction endpoint"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line, certifying that you wrote the change or otherwise have the right to submit it under the project's license. To sign off commits you already made, use `git rebase --signoff main`.

## Security issues

Do not report vulnerabilities in public issues. Follow [SECURITY.md](SECURITY.md).

## Project policies

- [GOVERNANCE.md](GOVERNANCE.md): how decisions are made and how to become a maintainer.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md): expected behaviour.
- [docs/stability.md](docs/stability.md): versioning, MSRV and what counts as public API.

## License

By contributing, you agree that your contributions are licensed under the Apache License 2.0.
