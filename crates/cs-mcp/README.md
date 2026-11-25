# cs-mcp

A [Model Context Protocol](https://modelcontextprotocol.io/specification/2026-07-28) server for Cognitive Substrate. Use it to give Claude Code, Cursor or any other MCP client governed, traceable memory.

It is built on the official Rust SDK, [`rmcp`](https://crates.io/crates/rmcp) 3.11, and speaks MCP **2026-07-28**. It also negotiates down to older protocol versions (2025-11-25 back to 2024-11-05) for clients that need them.

- **Transports**: stdio (the default) and Streamable HTTP (`--http`).
- **One principal**: the server acts for one tenant and one user. Everything it writes is private to that user, and it reads only what that user may read.
- **Errors are tool results**: validation, not-found, forbidden and conflict errors come back as MCP tool errors (`isError: true`) with the body `{"error": {"code": "...", "message": "..."}}`.
- **No content in logs**: logs go to stderr and contain identifiers only. Memory content is never logged.

## Install

```bash
cargo install --path crates/cs-mcp      # installs the `cs-mcp` binary
```

The crate requires Rust 1.88, because `rmcp` 3.x does. The rest of the workspace builds with Rust 1.85.

## Configure

| Variable | Default | |
|---|---|---|
| `CS_MCP_TENANT` / `CS_MCP_USER` | generated (in memory only) | The principal, as UUIDs. Required with `DATABASE_URL`. |
| `CS_MCP_TRANSPORT` | `stdio` | `stdio` or `http` (same as `--stdio` / `--http`) |
| `CS_MCP_BIND` | `127.0.0.1:8765` | HTTP listen address. The endpoint is `/mcp`. |
| `CS_MCP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Accepted `Host` headers (DNS-rebinding protection) |
| `CS_MCP_API_KEY` | — | Require `Authorization: Bearer <key>` over HTTP |
| `DATABASE_URL` | — | PostgreSQL. The in-memory store is used if this is unset. |
| `CS_ROLE` | `api` with Postgres, `all` in memory | `all` processes jobs (perception, resolution, backfill) in this process. With `api`, run `cognitive-substrate worker` alongside. |

All the service's other store, embedder and extractor variables work the same way: `CS_DB_MAX_CONNECTIONS`, `CS_AUTO_MIGRATE`, `CS_EMBEDDER*`, `CS_SHADOW_EMBEDDER*`, `CS_EXTRACTOR*`, `CS_RESOLUTION_CONFIG`, `CS_AUTO_RESOLVE`, `CS_EMBED_INLINE`, `CS_MIN_ACTIVE_COVERAGE` and `CS_WORKER_POLL_MS`. The one exception is `fastembed`, which is available only in the service binary. See [`crates/cs-server/src/main.rs`](../cs-server/src/main.rs).

If the identity variables are unset, the in-memory server generates both ids and prints them to stderr. That is fine for trying it out, but every restart starts empty.

### Claude Code

```bash
claude mcp add cognitive-substrate \
  --env CS_MCP_TENANT=$(uuidgen) --env CS_MCP_USER=$(uuidgen) \
  --env DATABASE_URL=postgres://cs:cs@localhost:5432/cs \
  -- cs-mcp
```

or in `.mcp.json`:

```json
{
  "mcpServers": {
    "cognitive-substrate": {
      "command": "cs-mcp",
      "env": {
        "CS_MCP_TENANT": "0193f0c4-0000-7000-8000-000000000001",
        "CS_MCP_USER": "0193f0c4-0000-7000-8000-000000000002",
        "DATABASE_URL": "postgres://cs:cs@localhost:5432/cs"
      }
    }
  }
}
```

### Cursor

`~/.cursor/mcp.json` (or `.cursor/mcp.json` in a project):

```json
{
  "mcpServers": {
    "cognitive-substrate": {
      "command": "cs-mcp",
      "args": [],
      "env": {
        "CS_MCP_TENANT": "0193f0c4-0000-7000-8000-000000000001",
        "CS_MCP_USER": "0193f0c4-0000-7000-8000-000000000002"
      }
    }
  }
}
```

Leave out `DATABASE_URL` to try it in memory.

### Streamable HTTP

```bash
CS_MCP_API_KEY=secret cs-mcp --http 127.0.0.1:8765   # http://127.0.0.1:8765/mcp
```

Requests that negotiate 2026-07-28 are served statelessly. Older clients get legacy sessions.

## Tools

| Tool | What it does |
|---|---|
| `remember` | Creates an **active** semantic memory with provenance `user_input`. Optional subject/predicate/object and idempotency key. |
| `recall` | Policy-controlled recall: `max_results`, `min_score`, `domains`, `include_candidates`, `include_beliefs`, and `as_of` (RFC 3339 transaction time). Returns ranked memories, relevant beliefs and the `trace_id`. |
| `get_recall_trace` | Shows a recorded recall: the resolved policy, data snapshot, and per-channel scores of the selected memories. |
| `replay_recall` | Exact replay against the substrate as it was. Reports `identical` plus any `added` or `removed` memories. |
| `observe` | Stores raw text as an immutable observation. With an in-process worker, it is processed immediately and the extracted **candidates** are returned. |
| `promote_memory` | Validates a candidate so that recall treats it as trusted. |
| `list_beliefs` | Current beliefs, newest first, optionally including retracted ones. |
| `assert_belief` | Creates a belief with an explicit confidence and interpretation, backed by `user_input` evidence. |
| `create_entities`, `create_relations`, `add_observations`, `search_nodes` | Compatible with the reference memory server (see below). |

Results are compact JSON text. A memory is shown as `{memory_id, revision_id, domain, status, text}`, where `text` is the memory's canonical index text.

## Mapping from the reference memory server

The reference [`@modelcontextprotocol/server-memory`](https://github.com/modelcontextprotocol/servers/tree/main/src/memory) keeps a mutable JSON knowledge graph. Here, the same calls become ordinary, versioned, private memories. They are traced on recall and can be replayed.

| Reference tool | Substrate mapping |
|---|---|
| `create_entities` | For each entity, a semantic memory `"<name> is a <entityType>"` (subject `name`, predicate `is_a`, object `entityType`), plus one semantic memory `"<name>: <observation>"` (subject `name`) per observation. Writes are idempotent: repeating a call returns the same memory ids. |
| `create_relations` | A relational memory (`from`, `relationType`, `to`, directed), also idempotent. |
| `add_observations` | One semantic memory `"<entityName>: <content>"` per content (subject `entityName`), also idempotent. |
| `search_nodes` | A traced `recall` of up to 20 memories. Hits are grouped into `entities` (`name`, `entityType`, `observations`) and `relations`. Other memories are listed separately, and the `trace_id` is returned. |
| `read_graph`, `open_nodes` | Not provided. Use `search_nodes` or `recall`, which are bounded and traced. |
| `delete_entities`, `delete_observations`, `delete_relations` | Not provided. Deletion in the substrate is an explicit tombstone or purge with a reason, available through the HTTP API. |

Entity names are matched exactly as given. The substrate's governed entity graph (aliases, merges, traversal) is built from the same structured fields and is available through the HTTP API.

## Tests

```bash
cargo test -p cs-mcp
```

The tests call the tool handlers directly against the in-memory store, with no transport and no network.
