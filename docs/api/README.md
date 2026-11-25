# HTTP API description

[`openapi.yaml`](openapi.yaml) is an [OpenAPI 3.1](https://spec.openapis.org/oas/v3.1.0) description of every `/v1` route served by `cs-server`. It lists each route's parameters, request body, success and error responses, and the authentication headers.

The file is written by hand. `crates/cs-server/tests/openapi.rs` checks it against the router (`crates/cs-server/src/lib.rs`), so `cargo test` fails when:

- a route is in the router but not in the spec, or in the spec but not served;
- an operation's path parameter, query parameters, request body, success status codes or success schema no longer match its handler;
- a `$ref` does not resolve, a schema is unused, or an `operationId` is used twice;
- the server can return an error code that the `Error` schema does not list.

Request and response schemas are written by hand from the Rust types in `cs-engine`, `cs-memory`, `cs-policy` and `cs-core`. The test checks schema **names** against handler signatures, not every field. When you change a serialized type, update its schema in the same change.

## Authentication

| Header | Required | In the spec |
|---|---|---|
| `x-cs-tenant` | Yes, on every route except `/v1/health` and `/v1/ready` | `tenantHeader` security scheme |
| `Authorization: Bearer <key>` | Only when the server sets `CS_API_KEY` | `bearerAuth` security scheme |
| `x-cs-user`, `x-cs-agent` | No (UUIDs) | Header parameters |
| `x-cs-projects` | No (comma-separated UUIDs) | Header parameter |

Every error body has the form `{"error": {"code": "...", "message": "..."}}`.

## Viewing the spec

Use any OpenAPI 3.1 viewer:

```bash
# Swagger UI at http://localhost:8081
docker run --rm -p 8081:8080 -e SWAGGER_JSON=/spec/openapi.yaml \
  -v "$PWD/docs/api:/spec" swaggerapi/swagger-ui

# Redoc as a single static HTML file
npx @redocly/cli build-docs docs/api/openapi.yaml -o api.html

# Lint
npx @redocly/cli lint docs/api/openapi.yaml
```

Editors with OpenAPI support (for example the VS Code OpenAPI extensions) can also preview the file directly.

## Generating clients

[openapi-generator](https://openapi-generator.tech/) produces typed clients from the spec:

```bash
# Python
docker run --rm -v "$PWD:/local" openapitools/openapi-generator-cli generate \
  -i /local/docs/api/openapi.yaml -g python -o /local/clients/python \
  --additional-properties=packageName=cognitive_substrate_client

# TypeScript (fetch)
docker run --rm -v "$PWD:/local" openapitools/openapi-generator-cli generate \
  -i /local/docs/api/openapi.yaml -g typescript-fetch -o /local/clients/typescript
```

For TypeScript types only, without a runtime client, use [`openapi-typescript`](https://openapi-ts.dev/):

```bash
npx openapi-typescript docs/api/openapi.yaml -o cognitive-substrate.d.ts
```

Generated clients send `x-cs-tenant` (and the bearer key, if the server needs one) through their security configuration. They send `x-cs-user`, `x-cs-agent` and `x-cs-projects` as optional per-call header parameters.

Several response types are tagged unions (for example `SourceRef` by `type`, `MemoryContent` by `domain`, `PredictionResult` by `result`), described with `oneOf` and `const`. Generators support these to different degrees. If one produces awkward types, try a generator with full OpenAPI 3.1 support.
