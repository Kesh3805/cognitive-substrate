# Landscape and growth strategy (November 2026)

This is a snapshot taken on 2026-11-25 by several parallel web-research agents, plus web-grounded answers from ChatGPT and Gemini. Star counts come from the GitHub API on that date.

Claims marked **[unverified]** come only from secondary sources or summaries and must be checked before anyone quotes them. Sources are listed at the end.

## Summary

- **The position to take:** the *governed system of record* for agent memory and decisions, not another memory framework.
- **The promise:** every belief has evidence, every recall has a trace, and every memory-driven decision can be audited and replayed.
- **What nobody else has in open source:**
  - exact and counterfactual replay of recall;
  - refuting evidence with revision lineage and dependency propagation;
  - a validation gate before facts enter memory;
  - preregistered benchmarks that publish negative results.
- **What is not unique:**
  - Bitemporal history: Graphiti has it.
  - Evidence-backed observations: Hindsight has them.
  - Audit logs: paid tiers elsewhere offer them.
- **The bottleneck is distribution, not capability.** There is no Python or TypeScript SDK, no MCP server, no framework adapter and no public-benchmark score. Every fast-growing Rust infrastructure project fixed those first.

## 1. Competitors

| Product | License | Stars | Funding | Bitemporal | Evidence / provenance | Replayable recall | Governance | Benchmark claims |
|---|---|---|---|---|---|---|---|---|
| Mem0 | Apache-2.0 | 69.5k | $24M (2025-10) | no | entity links | no | audit logs and on-prem only on a custom tier | LoCoMo 92.5, LongMemEval 94.4 (managed platform, not the OSS SDK) |
| Hindsight (vectorize-io) | MIT | 63.4k | [unverified] | timestamps, temporal search | observations keep supporting evidence and proof counts | no | isolated banks | LongMemEval state of the art (self-described as reproduced) |
| Cognee | Apache-2.0 | 33.6k | $7.5M seed (2026-02) | no; bitemporal in Enterprise [unverified] | chunk metadata | no | `forget` | BEAM 0.79 / 0.67 |
| Zep / Graphiti | Apache-2.0 (Graphiti) | 33.1k | about $2.3M, YC W24 | **yes** (invalidation) | **yes** (episode provenance) | no | SOC2, HIPAA, BYOC on Enterprise | DMR 94.8 |
| Supermemory | MIT wrapper, proprietary engine | 31.7k | about $3M (2025-10) | [unverified] | [unverified] | no | SOC2, HIPAA | — |
| Letta | Apache-2.0 | 25.5k | $10M seed (2024-09) | git history | commit messages | partial (diffs) | RBAC/SSO on Enterprise | — |
| Memori | non-standard | 17.6k | [unverified] | — | — | — | VPC / on-prem | — |
| MemOS | Apache-2.0 | 12.5k | [unverified] | no | no | no | cube isolation | LoCoMo 88.8, LongMemEval 89.2 (self-reported) |
| Memobase | Apache-2.0 | 3.0k (inactive since 2026-01) | — | event timeline | no | no | — | — |
| LangMem | MIT | 1.8k | LangChain | no | no | no | — | — |

**Built-in platform memory** covers basic chat memory, and that part is commoditized:
- ChatGPT and Claude memory;
- AWS Bedrock AgentCore Memory (GA October 2025) and Vertex AI Memory Bank [unverified];
- Databricks Agent Memory (2026-10-20) [unverified].

**Newly funded entrants, not open source:** Graphon ($35M Series A, 2026-11) tracks the source and the time of change for each item. Engram ($98M, 2026-06) puts memory into model weights.

**The most dangerous direct competitor is Hindsight, not Mem0.** It already sells "beliefs with evidence" to the same developers, has 63k stars, and ships adapters for many frameworks.

## 2. What we have, what is copyable, what is not

| Differentiator | Anyone else? | How hard to copy |
|---|---|---|
| Exact and counterfactual replay of recall, with traces and deterministic policy | No one documents it | **High.** Incumbents' write paths are stochastic and mutable, and replay needs determinism end to end. |
| Beliefs with supporting **and refuting** evidence, lineage and dependency propagation | Partial (Hindsight: supporting evidence; Graphiti: invalidation) | Medium |
| A validation gate before memory (candidates) | No (every open-source system writes extracted facts directly) | Low |
| Bitemporal history together with hard purge and an append-only audit log, open source on plain Postgres | Graphiti is bitemporal, but purge and audit are not documented | Low to medium |
| Answer / investigate / abstain | — | Low |
| Causal hypotheses from randomized evidence; sequential prediction monitoring | No one | High, but buyers don't search for it by name |
| Preregistered benchmarks with negative results | No one (every claim found was vendor-reported) | High (it is a trust position, not code) |

**Messaging.** Buyers do not shop for "causal bounds" or "e-detectors". Lead with *provable auditability* and *no silent stale or invented memories*. Present the statistics as how the guarantees are kept.

**A tension to resolve publicly.** Immutable history, hard purge and exact replay pull against each other. After a purge, a recall that used the purged memory cannot be replayed exactly. The project should specify what replay guarantees after a purge, and say so before a reviewer does.

## 3. Market signals

- **Governance is the blocker.**
  - Gartner (2025-06-25) predicts that over 40% of agentic AI projects will be cancelled by the end of 2027, citing costs, unclear value and inadequate risk controls.
  - About 80% of organizations lack mature agentic-AI governance (Deloitte 2026) [unverified].
- **Regulation.** These obligations *support the need for* the project's guarantees; the project does not "ensure compliance".
  - EU AI Act: Art. 12 requires automatic logging, and Art. 19 and 26(6) require keeping logs at least 6 months. The Digital Omnibus moved Annex III high-risk obligations to **2 December 2027** [regulation number unverified]. That leaves about 12 months to mature before procurement cycles fix their requirements.
  - GDPR Art. 17: the EDPB (Opinion 28/2024) holds that models trained on personal data are not anonymous by default. Embeddings can be inverted (vec2text recovers 92% of 32-token inputs), so embeddings of personal text should be treated as personal data.
- **Trust in benchmarks has collapsed.**
  - A LoCoMo audit found 6.4% of answer keys wrong, and its standard judge accepted 63% of deliberately wrong answers.
  - Vendor LongMemEval claims have not reproduced independently [single sources].
  - A credible, preregistered evaluator would stand out.

## 4. Benchmarks to run

| Benchmark | License | Why | Notes |
|---|---|---|---|
| **LongMemEval-S** | MIT | Most comparable (Zep 71.2% with GPT-4o); covers knowledge updates, temporal reasoning and abstention (`_abs`) | Data was cleaned in September 2025, so state the version. V2 (May 2026) is agentic. |
| **BEAM** (ICLR 2026) | [unverified] | 100K–10M tokens, which defeats the full-context shortcut; covers contradictions and event ordering | Mem0 reports 64.1 / 48.6 |
| **MemoryAgentBench** FactConsolidation | — | Multi-hop conflict resolution sits at 7% or below for every system | Synthetic forgetting tasks |
| AfterDeleteBench-style deletion audit (arXiv 2610.31366) | — | Compares an exposed-then-deleted agent with one never exposed; provenance-aware deletion passes it | No code released yet |
| LoCoMo | **CC BY-NC** | Secondary only | Publish the audited answer-key ceiling |

**Work required:**
- ingestion adapters that map session timestamps to valid time and ingestion order to transaction time;
- a fixed LLM answering layer, with model, prompt and context budget preregistered;
- an abstention mapping that counts *investigate* as abstain;
- full-context and naive-RAG baselines run through the same answering layer.

**How to report:**
- Use the official judge prompt, with at least 3 repeats and a second judge model.
- Report exact match or F1 alongside the judge score.
- Publish raw answers, judge verdicts and replay logs.
- Report cost and latency.
- Never place vendor self-reports beside our numbers as if they were measured the same way.

**Benchmarks nobody has, which this project could publish:**
1. bitemporal "as-of" QA: what did the system believe at time T about time V;
2. three-way calibrated abstention scored as risk–coverage;
3. replay and audit determinism;
4. verifiable erasure, meaning purge certificates covering embeddings.

## 5. Integrations

Most framework memory interfaces have 2–6 methods. Ranked by impact against effort:

1. **An MCP server** on `rmcp` (the official Rust SDK, v3.11, weekly releases), targeting spec 2026-07-28 (stateless, Streamable HTTP).
   - Expose the reference memory-server tool names (`create_entities`, `add_observations`, `search_nodes`) plus `recall`, `beliefs` and `hypotheses`.
   - Publish to the official MCP Registry from CI.
   - Mem0 reportedly retired its local MCP server [unverified], which leaves room for a serious self-hostable one.
2. **An OpenAPI spec** (e.g. utoipa) plus a **Python HTTP SDK**.
3. A **TypeScript SDK**.
4. A **LangGraph `BaseStore`** that is LangMem-compatible.
5. **Vercel AI SDK tools,** plus a pull request to its memory-providers page, which works as a free discovery channel.
6. A **Google ADK `BaseMemoryService`** and an **OpenAI Agents SDK `Session`**.
7. **CrewAI `StorageBackend`** and **LlamaIndex `BaseMemoryBlock`**.
8. A **PyO3 embedded package** for local-first, no-server use (the LanceDB and Qdrant Edge pattern), once demand is shown.

Hindsight shows the pattern: it shipped adapters quickly and is listed on the frameworks' own memory pages.

## 6. Growth: what worked for comparable Rust projects

**Apache Iggy:**
- It started as a personal learning project (2023-03).
- One long, honest technical post (2023-12) reached Hacker News and Reddit. Its author calls that post the catalyst.
- Community members then wrote SDKs in 7+ languages.
- A sponsoring company (LaserData) followed, then the Thoughtworks Radar (Assess), then relicensing from MIT to Apache-2.0 (2025-01).
- It entered the Apache Incubator with 2.2k stars and 14 initial committers (2025-02), and published a public benchmark dashboard.
- It graduated to a top-level project after about 18 months (2026-08), with 4.5k+ stars, 120+ contributors and 500k+ downloads. It has 5.35k stars today.
- **Incubation brought trust and governance, not viral growth:** contributors grew fourfold, stars twofold.

**Others:**
- Polars (40.5k) and Qdrant (35.7k): a reproducible benchmark against the incumbent, plus Python bindings.
- LanceDB (12k): an embedded mode, with LangChain and LlamaIndex integrations built by the community.
- Meilisearch (60k): docs writers, developer relations and an integrations team.
- SurrealDB (33.4k): reached 10k stars four weeks after a big launch.

The fast growers all had a company or cloud offering paying for docs and developer relations.

**Foundations:**
- Apache graduation needs at least 3 legally independent committers.
- LF AI & Data incubation needs contributors from 2+ organizations, about 300 stars and an OpenSSF Silver badge. It is the best topical fit.
- A CNCF application is possible but fits poorly.
- Apply after 3–5 regular outside contributors from at least 2 organizations exist, which is likely 12–18 months away.

## 7. Plan

**Next 90 days, in order of expected impact:**
1. **MCP server** plus a one-command install for Claude Code and Cursor, listed in the registry.
2. **OpenAPI, plus Python and TypeScript SDKs.** Then the LangGraph and OpenAI Agents SDK adapters.
3. **A 5-minute flagship demo.**
   - Story: an agent makes a wrong decision; we trace it to the belief responsible, revise that belief, and replay the decision under the corrected belief and an alternative policy.
   - Include a verifiable purge (GDPR Art. 17) and the Art. 12 decision trace.
4. **A preregistered public evaluation.** LongMemEval-S first, with raw outputs and replay logs, published on a results page that is easy to reproduce.
5. **One long, honest technical post** ("why agent memory needs a system of record"), including our negative results. Share it on Show HN, r/rust and r/LocalLLaMA.
6. **Trust basics:**
   - `SECURITY.md` with private reporting;
   - an OpenSSF Scorecard and a Best Practices badge (Passing first);
   - an SBOM and signed release artifacts;
   - `cargo-deny`;
   - a SemVer, MSRV and stability policy;
   - `GOVERNANCE.md` and `CONTRIBUTING.md`, with a DCO rather than a CLA;
   - 20+ good-first issues;
   - a Discord.

**12 months:**
- Remaining framework adapters and the PyO3 embedded package.
- BEAM and deletion-audit results.
- Papers or benchmarks on bitemporal belief revision, verifiable erasure, and anytime-valid effect gating (gap evidence in §4 and the sources).
- 3–5 design partners in regulated industries, with 2–3 public case studies.
- 3+ outside committers.
- Talks at RustConf, Postgres conferences and AI Engineer, and a Thoughtworks Radar submission.
- Then LF AI & Data Sandbox (or an Apache proposal).
- Suggested targets: 5,000 stars, 20 outside contributors, 10 production deployments, 3 public case studies. Deployments are the metric that matters.

**Sustainability.**
- Stay **Apache-2.0**. Every relicensing to BSL or SSPL (Redis, Elastic, HashiCorp) produced a fork, and two of the three came back to an OSI license.
- If revenue is needed, sell hosting, compliance packs (audit export, retention, SSO) and support.
- Keep the governance core open.
- Non-dilutive options: GitHub Secure Open Source Fund ($10k, rolling), NLnet (next deadline 2027-01-03), the OpenAI Codex Open Source Fund (API credits), AWS open-source credits. The Sovereign Tech Fund comes later, once adoption is shown.

## 8. Research that supports, or challenges, the design

| Design choice | Support | Challenge |
|---|---|---|
| Bitemporal memory | LongMemEval: knowledge-update and temporal questions cost 30–60% accuracy; Graphiti's gains | Bitemporality alone is not novel. Retrieval-time "as-of" queries are the open gap (arXiv 2601.07468 [unverified]). |
| Dependency propagation | RippleEdits: editors miss facts that follow from an edit | That evidence is about weight editing; no one has yet shown external dependency graphs beat in-context editing |
| Abstention | Sufficient Context (ICLR 2025): a sufficiency signal improves selective accuracy | Gains are modest (2–10%), and public abstention sets are tiny (LongMemEval has 30 items) |
| Randomized causal adjudication; bounds outside the LLM | LLMs err above 25% on counterfactual PN/PS questions (arXiv 2408.08210); agents design poor interventions [unverified 2026 preprints] | Our own experiment 013: causal gating did not beat simpler per-context baselines |
| Anytime-valid monitoring | E-process monitoring of agents and deployments [unverified 2026 preprints] | Calibrated thresholds weaken anytime guarantees [unverified 2026 preprint] |
| Write-time provenance | No retrieval-time filter without provenance can certify against adaptive memory injection [unverified 2026 preprint] | — |

## Sources

**Market:**
- https://techcrunch.com/2025/10/28/mem0-raises-24m-from-yc-peak-xv-and-basis-set-to-build-the-memory-layer-for-ai-apps
- https://mem0.ai/pricing
- https://github.com/mem0ai/mem0
- https://www.getzep.com/pricing
- https://github.com/getzep/graphiti
- https://arxiv.org/abs/2501.13956
- https://www.cognee.ai/blog/cognee-news/cognee-raises-seven-million-five-hundred-thousand-dollars-seed
- https://github.com/topoteretes/cognee
- https://supermemory.ai/pricing
- https://github.com/MemTensor/MemOS
- https://github.com/memodb-io/memobase
- https://github.com/vectorize-io/hindsight
- https://github.com/MemoriLabs/Memori
- https://github.com/langchain-ai/langmem
- https://docs.letta.com/letta-code/pricing
- https://help.openai.com/en/articles/8590148-memory-in-chatgpt
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool
- https://thenextweb.com/news/graphon-labs-35m-series-a-ai-agent-memory
- https://dealroom.co/news/135964-engram-emerges-from-stealth-with-98m-to-build-a-learned-memory-layer-for/
- https://mnemoverse.com/docs/library/ai-memory-solutions-2026-q3 [secondary]

**Signals and regulation:**
- https://www.gartner.com/en/newsroom/press-releases/2025-06-25-gartner-predicts-over-40-percent-of-agentic-ai-projects-will-be-canceled-by-end-of-2027
- https://langfuse.com/resources/engineering/eu-ai-act-record-keeping
- https://labs.cloudsecurityalliance.org/research/csa-research-note-eu-ai-act-omnibus-vii-deadline-delay-20260/
- https://www.gibsondunn.com/eu-ai-act-omnibus-agreement-postponed-high-risk-deadlines-and-other-key-changes/
- https://edpb.europa.eu/system/files/2024-12/edpb_opinion_202428_ai-models_en.pdf
- https://arxiv.org/abs/2310.06816
- https://nvlpubs.nist.gov/nistpubs/ai/NIST.AI.600-1.pdf

**Benchmarks:**
- https://arxiv.org/abs/2410.10813
- https://github.com/xiaowu0162/LongMemEval
- https://arxiv.org/abs/2510.27246
- https://huggingface.co/datasets/Mohammadta/BEAM
- https://arxiv.org/abs/2507.05257
- https://github.com/snap-research/locomo
- https://dev.to/penfieldlabs/we-audited-locomo-64-of-the-answer-key-is-wrong-and-the-judge-accepts-up-to-63-of-intentionally-33lg
- https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/
- https://arxiv.org/abs/2610.31366 [unverified]

**Integrations:**
- https://modelcontextprotocol.io/specification/2026-07-28/changelog
- https://github.com/modelcontextprotocol/rust-sdk/releases
- https://modelcontextprotocol.io/registry/quickstart
- https://reference.langchain.com/python/langgraph.store/base/BaseStore
- https://developers.llamaindex.ai/python/framework/module_guides/deploying/agents/memory/
- https://docs.crewai.com/concepts/memory
- https://openai.github.io/openai-agents-python/sessions/
- https://adk.dev/sessions/memory/
- https://ai-sdk.dev/docs/agents/memory

**Growth:**
- https://iggy.apache.org/blogs/2023/12/29/building-message-streaming-in-rust/
- https://iggy.apache.org/blogs/2024/05/29/one-year-of-building-the-message-streaming/
- https://iggy.apache.org/blogs/2025/02/17/transparent-benchmarks
- https://iggy.apache.org/blogs/2026/08/24/apache-iggy-top-level-project-tlp-graduation/
- https://incubator.apache.org/projects/iggy.html
- https://incubator.apache.org/guides/graduation.html
- https://lfaidata.foundation/?p=4789
- https://pola.rs/posts/i-wrote-one-of-the-fastest-dataframe-libraries
- https://surrealdb.com/blog/10000-thank-yous
- https://www.meilisearch.com/blog/how-meilisearch-became-open-source-a-retrospective

**Sustainability:**
- https://www.theregister.com/2025/05/01/redis_returns_to_open_source/
- https://www.cncf.io/projects/opentofu/
- https://github.com/open-source/github-secure-open-source-fund
- https://nlnet.nl/news/2026/20260803-phaseshift.html
- https://openai.com/form/codex-open-source-fund/
- https://github.com/ossf/scorecard
- https://www.bestpractices.dev/criteria/0

**Research:**
- https://arxiv.org/abs/2307.12976
- https://arxiv.org/abs/2411.06037
- https://arxiv.org/abs/2408.08210
