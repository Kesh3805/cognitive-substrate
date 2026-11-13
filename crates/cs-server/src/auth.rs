//! Caller authentication.
//!
//! M2 ships a deliberately small integration point: an optional shared
//! bearer key plus principal headers. Production deployments are expected to
//! place an identity-aware proxy in front of the service (or replace this
//! extractor) that sets the principal headers from a verified token.
//!
//! | Header           | Meaning                                  |
//! |------------------|------------------------------------------|
//! | `x-cs-tenant`    | Tenant UUID (required)                   |
//! | `x-cs-user`      | User UUID                                |
//! | `x-cs-agent`     | Agent UUID                               |
//! | `x-cs-projects`  | Comma-separated authorized project UUIDs |

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, StatusCode, request::Parts},
};
use cs_core::{AccessContext, AgentId, ProjectId, TenantId, UserId};
use uuid::Uuid;

use crate::{ApiError, AppState};

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// When set, every `/v1` request except health/readiness must present
    /// `Authorization: Bearer <api_key>`.
    pub api_key: Option<String>,
}

/// The authenticated principal.
#[derive(Debug, Clone)]
pub struct Caller(pub AccessContext);

fn unauthorized(msg: &str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", msg)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn header_uuid(headers: &HeaderMap, name: &str) -> Result<Option<Uuid>, ApiError> {
    match headers.get(name) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| Uuid::parse_str(s.trim()).ok())
            .map(Some)
            .ok_or_else(|| unauthorized(&format!("{name} must be a UUID"))),
    }
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let headers = &parts.headers;
        if let Some(expected) = &state.auth.api_key {
            let presented = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default();
            if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
                return Err(unauthorized("missing or invalid API key"));
            }
        }
        let tenant = header_uuid(headers, "x-cs-tenant")?
            .ok_or_else(|| unauthorized("x-cs-tenant header is required"))?;
        let mut projects = Vec::new();
        if let Some(v) = headers.get("x-cs-projects") {
            let raw = v
                .to_str()
                .map_err(|_| unauthorized("invalid x-cs-projects"))?;
            for p in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                projects.push(ProjectId(
                    Uuid::parse_str(p).map_err(|_| unauthorized("invalid x-cs-projects"))?,
                ));
            }
        }
        Ok(Self(AccessContext {
            tenant_id: TenantId(tenant),
            user_id: header_uuid(headers, "x-cs-user")?.map(UserId),
            agent_id: header_uuid(headers, "x-cs-agent")?.map(AgentId),
            project_ids: projects,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secrets"));
    }
}
