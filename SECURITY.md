# Security policy

## Supported versions

Cognitive Substrate is pre-1.0. Security fixes are made only on the latest `main` and, once releases exist, the latest release. Older commits and releases are not patched.

## Reporting a vulnerability

Please report vulnerabilities **privately** through GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Do not open a public issue, pull request or discussion for a suspected vulnerability.

Include what you can: the affected component (crate, endpoint, migration), the commit or version, reproduction steps, and the impact you expect.

## What to expect

The project has a single maintainer, so these are best-effort targets, not guarantees:

- acknowledgement within 7 days;
- an initial assessment within 14 days;
- a fix or mitigation plan agreed with you before any public disclosure.

We will credit you in the advisory unless you ask us not to. Please give us a reasonable chance to ship a fix before disclosing publicly.

## Scope

Cognitive Substrate is a memory layer: it stores information about people and organizations. In addition to the usual classes (injection, authentication bypass, denial of service, dependency vulnerabilities), the following are in scope and especially important:

- **Tenant and scope isolation.** Any read, recall, replay, event, trace or job that exposes a record outside the caller's tenant or scope (see [scope and access](docs/specifications/scope-and-access.md)), or that lets a caller distinguish "unreadable" from "missing".
- **Purge completeness.** A purge that leaves content, revisions, embeddings, derived edges or derived memories behind, or a background job that re-creates them (ADR-0010, ADR-0014).
- **Content leakage through the event log.** Event payloads must contain identifiers and hashes only (ADR-0006). Any payload that carries memory, belief, evidence or observation content is a vulnerability.
- **Validation bypass.** Model-derived or extracted content becoming trusted knowledge without an explicit validation path.

Out of scope: vulnerabilities in a deployment's own infrastructure (database, network, secrets management), and the fixed example credentials in `deploy/compose.yaml`, which is for local development only.
