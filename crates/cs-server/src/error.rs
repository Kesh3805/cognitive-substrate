//! Structured API errors: `{"error": {"code": "...", "message": "..."}}`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<cs_core::Error> for ApiError {
    fn from(e: cs_core::Error) -> Self {
        use cs_core::Error as E;
        let status = match &e {
            E::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            E::NotFound { .. } => StatusCode::NOT_FOUND,
            E::Forbidden(_) => StatusCode::FORBIDDEN,
            E::Conflict(_) => StatusCode::CONFLICT,
            E::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if status.is_server_error() {
            // Never leak storage internals to clients.
            tracing::error!(error = %e, "storage failure");
            "internal storage failure".to_owned()
        } else {
            e.to_string()
        };
        Self {
            status,
            code: e.code(),
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}
