//! Keeps `docs/api/openapi.yaml` in lockstep with the router.
//!
//! The spec is hand-written; these tests make drift fail CI:
//!
//! * every route registered in `src/lib.rs` is in the spec, and every spec
//!   operation is a route (checked from the source and against the live
//!   router);
//! * each operation's path parameter, query parameters, request body,
//!   success status codes and success schema match its handler's signature;
//! * every `$ref` resolves, operation ids are unique, authenticated routes
//!   document the auth headers and error responses, and the error schema
//!   lists every error code the server can emit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cs_engine::CognitiveEngine;
use cs_server::{AppState, AuthConfig, router};
use cs_storage::InMemoryStore;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const SPEC: &str = include_str!("../../../docs/api/openapi.yaml");
const LIB: &str = include_str!("../src/lib.rs");
const HTTP_METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// Routes that need no caller identity.
const PUBLIC: [&str; 2] = ["/v1/health", "/v1/ready"];

fn spec() -> Value {
    serde_yaml_ng::from_str(SPEC).expect("docs/api/openapi.yaml is valid YAML")
}

/// One registered route: `(METHOD, full path)` and its handler function.
#[derive(Debug, Clone)]
struct Route {
    method: String,
    path: String,
    handler: String,
}

/// Reads the string literal starting at the first `"` in `s`.
fn string_literal(s: &str) -> (&str, &str) {
    let start = s.find('"').expect("a string literal") + 1;
    let len = s[start..].find('"').expect("a closing quote");
    (&s[start..start + len], &s[start + len + 1..])
}

/// The text up to the parenthesis closing an already-open one.
fn until_close(s: &str) -> &str {
    let mut depth = 1_u32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced parentheses in route registration");
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every route registered by `router()` in `src/lib.rs`, with the `/v1`
/// nest prefix applied.
fn router_routes() -> Vec<Route> {
    let nests: Vec<_> = LIB.match_indices(".nest(").collect();
    assert_eq!(nests.len(), 1, "the parser expects exactly one .nest()");
    let (prefix, _) = string_literal(&LIB[nests[0].0..]);
    let mut routes = Vec::new();
    for (at, m) in LIB.match_indices(".route(") {
        let rest = &LIB[at + m.len()..];
        let (path, after) = string_literal(rest);
        let args = until_close(after);
        let mut found = 0;
        for method in HTTP_METHODS {
            let needle = format!("{method}(");
            for (i, _) in args.match_indices(&needle) {
                if args[..i].chars().next_back().is_some_and(is_ident) {
                    continue;
                }
                let handler: String = args[i + needle.len()..]
                    .chars()
                    .take_while(|c| is_ident(*c))
                    .collect();
                routes.push(Route {
                    method: method.to_uppercase(),
                    path: format!("{prefix}{path}"),
                    handler,
                });
                found += 1;
            }
        }
        assert!(found > 0, "no method router found for route {path}");
    }
    assert!(!routes.is_empty(), "no routes parsed from src/lib.rs");
    routes
}

/// `(METHOD, path)` -> operation object.
fn spec_operations(spec: &Value) -> BTreeMap<(String, String), Value> {
    let mut ops = BTreeMap::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        for (key, op) in item.as_object().expect("path item") {
            if HTTP_METHODS.contains(&key.as_str()) {
                ops.insert((key.to_uppercase(), path.clone()), op.clone());
            }
        }
    }
    ops
}

fn resolve<'a>(spec: &'a Value, v: &'a Value) -> &'a Value {
    match v.get("$ref").and_then(Value::as_str) {
        Some(r) => spec
            .pointer(r.strip_prefix('#').expect("local $ref"))
            .unwrap_or_else(|| panic!("unresolved $ref {r}")),
        None => v,
    }
}

/// The handler's source text: from its `async fn` to the next item.
fn handler_source(name: &str) -> &'static str {
    let start = LIB
        .find(&format!("async fn {name}("))
        .unwrap_or_else(|| panic!("handler {name} not found in src/lib.rs"));
    let rest = &LIB[start..];
    let end = ["\nasync fn ", "\nfn ", "\n#[derive", "\n// "]
        .iter()
        .filter_map(|m| rest[1..].find(m).map(|i| i + 1))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The last path segment of a Rust type, e.g. `cs_memory::Job` -> `Job`.
fn type_name(t: &str) -> String {
    t.trim().rsplit("::").next().unwrap_or_default().to_owned()
}

/// The text between `open` and its matching `>`.
fn generic_arg<'a>(s: &'a str, open: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let mut depth = 1_u32;
    for (i, c) in s[start..].char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A schema rendered as a comparable name: `Job` or `[Job]`.
fn schema_name(s: &Value) -> String {
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        return r.rsplit('/').next().unwrap_or_default().to_owned();
    }
    if s.get("type").and_then(Value::as_str) == Some("array") {
        return format!("[{}]", schema_name(&s["items"]));
    }
    format!("{s}")
}

fn rust_response_name(src: &str) -> Option<String> {
    let t = generic_arg(src, "-> ApiResult<")?;
    Some(match generic_arg(t, "Vec<") {
        Some(inner) => format!("[{}]", type_name(inner)),
        None => type_name(t),
    })
}

#[test]
fn spec_is_openapi_3_1() {
    let spec = spec();
    assert_eq!(spec["openapi"], "3.1.0");
    assert!(spec["info"]["title"].is_string());
    assert!(spec["info"]["version"].is_string());
}

#[test]
fn routes_are_only_registered_in_lib_rs() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n == "lib.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains(".route(") && !text.contains(".nest("),
            "{} registers routes; extend tests/openapi.rs to parse it",
            path.display()
        );
    }
}

#[test]
fn every_router_route_is_in_the_spec_and_vice_versa() {
    let spec = spec();
    let documented: BTreeSet<_> = spec_operations(&spec).into_keys().collect();
    let registered: BTreeSet<_> = router_routes()
        .into_iter()
        .map(|r| (r.method, r.path))
        .collect();
    let missing: Vec<_> = registered.difference(&documented).collect();
    let extra: Vec<_> = documented.difference(&registered).collect();
    assert!(
        missing.is_empty(),
        "routes missing from docs/api/openapi.yaml: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "docs/api/openapi.yaml documents routes the router does not serve: {extra:?}"
    );
}

/// Sends `method path` with no credentials to the live router and reports
/// whether a route matched (anything but the 404 fallback or a 405).
async fn routed(app: &axum::Router, method: &str, path: &str) -> bool {
    let uri = path.replace("{id}", "00000000-0000-0000-0000-000000000000");
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let fallback = status == StatusCode::NOT_FOUND
        && serde_json::from_slice::<Value>(&body)
            .is_ok_and(|v| v["error"]["message"] == "no such route");
    !(fallback || status == StatusCode::METHOD_NOT_ALLOWED)
}

#[tokio::test]
async fn every_spec_operation_is_served_by_the_live_router() {
    let app = router(AppState::new(
        CognitiveEngine::new(Arc::new(InMemoryStore::new())),
        AuthConfig::default(),
    ));
    // The probe must tell served routes from unserved ones.
    assert!(!routed(&app, "GET", "/v1/no-such-route").await);
    assert!(!routed(&app, "DELETE", "/v1/memories/{id}").await);

    let spec = spec();
    let mut unserved = Vec::new();
    for (method, path) in spec_operations(&spec).into_keys() {
        if !routed(&app, &method, &path).await {
            unserved.push(format!("{method} {path}"));
        }
    }
    assert!(
        unserved.is_empty(),
        "not served by the router: {unserved:?}"
    );
}

#[test]
fn operations_match_their_handlers() {
    let spec = spec();
    let ops = spec_operations(&spec);
    let mut problems = Vec::new();
    for route in router_routes() {
        let key = (route.method.clone(), route.path.clone());
        let Some(op) = ops.get(&key) else { continue };
        let src = handler_source(&route.handler);
        let id = format!("{} {} ({})", route.method, route.path, route.handler);

        // Parameters declared on the operation or the path item.
        let item = &spec["paths"][&route.path];
        let params: Vec<&Value> = item["parameters"]
            .as_array()
            .into_iter()
            .chain(op["parameters"].as_array())
            .flatten()
            .map(|p| resolve(&spec, p))
            .collect();
        let has = |loc: &str| params.iter().any(|p| p["in"] == loc);

        if src.contains("Path(") != route.path.contains("{id}") {
            problems.push(format!("{id}: path extractor and `{{id}}` disagree"));
        }
        if route.path.contains("{id}")
            && !params
                .iter()
                .any(|p| p["in"] == "path" && p["name"] == "id" && p["required"] == true)
        {
            problems.push(format!("{id}: `id` path parameter not declared"));
        }
        if src.contains("Query<") != has("query") {
            problems.push(format!("{id}: query extractor and spec disagree"));
        }

        // Request body: `ApiJson<T>` is required, raw `Bytes` is optional.
        let body = &op["requestBody"];
        let expected_body = if let Some(t) = generic_arg(src, "ApiJson<") {
            Some((type_name(t), true))
        } else if src.contains("axum::body::Bytes") {
            let t = src
                .split("let req: ")
                .nth(1)
                .and_then(|s| s.split(' ').next())
                .expect("optional body handlers declare `let req: T`");
            Some((type_name(t), false))
        } else {
            None
        };
        match (&expected_body, body.is_null()) {
            (None, true) => {}
            (None, false) => problems.push(format!("{id}: spec has a body the handler ignores")),
            (Some(_), true) => problems.push(format!("{id}: request body not documented")),
            (Some((name, required)), false) => {
                let schema = &body["content"]["application/json"]["schema"];
                if schema_name(schema) != *name {
                    problems.push(format!("{id}: request body should be {name}"));
                }
                if body["required"].as_bool().unwrap_or(false) != *required {
                    problems.push(format!(
                        "{id}: request body `required` should be {required}"
                    ));
                }
            }
        }

        // Success status codes.
        let mut codes = BTreeSet::new();
        if src.contains("created(") || src.contains("StatusCode::CREATED") {
            codes.insert("201");
        }
        if src.contains("StatusCode::ACCEPTED") {
            codes.insert("202");
        }
        if src.contains("StatusCode::NO_CONTENT") {
            codes.insert("204");
        }
        if src.contains("\n    ok(") || src.contains("StatusCode::OK") || codes.is_empty() {
            codes.insert("200");
        }
        let responses = op["responses"].as_object().expect("responses");
        let documented: BTreeSet<&str> = responses
            .keys()
            .map(String::as_str)
            .filter(|c| c.starts_with('2'))
            .collect();
        if documented != codes {
            problems.push(format!(
                "{id}: success codes {documented:?}, handler returns {codes:?}"
            ));
        }

        // Success schema.
        if let Some(name) = rust_response_name(src) {
            for code in &codes {
                let schema =
                    &resolve(&spec, &responses[*code])["content"]["application/json"]["schema"];
                if schema_name(schema) != name {
                    problems.push(format!("{id}: {code} schema should be {name}"));
                }
            }
        }

        // Authentication and errors.
        let public = PUBLIC.contains(&route.path.as_str());
        if public {
            if op["security"] != Value::Array(vec![]) {
                problems.push(format!("{id}: public route must declare `security: []`"));
            }
        } else {
            for header in ["x-cs-user", "x-cs-agent", "x-cs-projects"] {
                if !params
                    .iter()
                    .any(|p| p["in"] == "header" && p["name"] == header)
                {
                    problems.push(format!("{id}: `{header}` header not documented"));
                }
            }
            for code in ["401", "403", "422", "500"] {
                if !responses.contains_key(code) {
                    problems.push(format!("{id}: {code} response not documented"));
                }
            }
            if (has("query") || expected_body.is_some()) && !responses.contains_key("400") {
                problems.push(format!("{id}: 400 response not documented"));
            }
            if route.path.contains("{id}") && !responses.contains_key("404") {
                problems.push(format!("{id}: 404 response not documented"));
            }
        }
        for (code, r) in responses {
            if !code.starts_with('2') {
                let schema = &resolve(&spec, r)["content"]["application/json"]["schema"];
                if schema_name(schema) != "Error" {
                    problems.push(format!("{id}: {code} must use the Error schema"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "spec drift:\n{}", problems.join("\n"));
}

#[test]
fn security_schemes_cover_the_auth_headers() {
    let spec = spec();
    let schemes = &spec["components"]["securitySchemes"];
    assert_eq!(schemes["tenantHeader"]["in"], "header");
    assert_eq!(schemes["tenantHeader"]["name"], "x-cs-tenant");
    assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");
    let global = spec["security"].as_array().expect("global security");
    assert!(
        global.iter().all(|r| r.get("tenantHeader").is_some()),
        "every global security requirement needs x-cs-tenant"
    );
}

#[test]
fn operation_ids_are_unique() {
    let spec = spec();
    let mut seen = BTreeSet::new();
    for ((method, path), op) in spec_operations(&spec) {
        let id = op["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
        assert!(seen.insert(id.to_owned()), "duplicate operationId {id}");
    }
}

fn collect_refs<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                out.push(r);
            }
            m.values().for_each(|x| collect_refs(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

#[test]
fn every_ref_resolves_and_every_schema_is_used() {
    let spec = spec();
    let mut refs = Vec::new();
    collect_refs(&spec, &mut refs);
    for r in &refs {
        let pointer = r.strip_prefix('#').expect("only local $refs");
        assert!(spec.pointer(pointer).is_some(), "unresolved $ref {r}");
    }
    let used: BTreeSet<_> = refs.iter().collect();
    for name in spec["components"]["schemas"].as_object().unwrap().keys() {
        let r = format!("#/components/schemas/{name}");
        assert!(used.contains(&r.as_str()), "unused schema {name}");
    }
}

#[test]
fn error_schema_lists_every_error_code() {
    let spec = spec();
    let documented: BTreeSet<&str> = spec["components"]["schemas"]["Error"]["properties"]["error"]
        ["properties"]["code"]["enum"]
        .as_array()
        .expect("error code enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut emitted: BTreeSet<String> = [
        cs_core::Error::validation("x"),
        cs_core::Error::not_found("x", "x"),
        cs_core::Error::forbidden("x"),
        cs_core::Error::conflict("x"),
        cs_core::Error::storage("x"),
    ]
    .iter()
    .map(|e| e.code().to_owned())
    .collect();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default();
        for (at, m) in text.match_indices("ApiError::new(") {
            let (code, _) = string_literal(&text[at + m.len()..]);
            emitted.insert(code.to_owned());
        }
    }
    let missing: Vec<_> = emitted
        .iter()
        .filter(|c| !documented.contains(c.as_str()))
        .collect();
    assert!(missing.is_empty(), "undocumented error codes: {missing:?}");
}
