//! A single error vocabulary shared across the engine.

/// Errors surfaced by the substrate. Each variant maps to a stable,
/// transport-independent error code (see [`Error::code`]).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("validation failed: {0}")]
    Validation(String),

    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("storage failure: {0}")]
    Storage(String),
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn not_found(kind: &'static str, id: impl ToString) -> Self {
        Self::NotFound {
            kind,
            id: id.to_string(),
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn storage(msg: impl ToString) -> Self {
        Self::Storage(msg.to_string())
    }

    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation_failed",
            Self::NotFound { .. } => "not_found",
            Self::Forbidden(_) => "forbidden",
            Self::Conflict(_) => "conflict",
            Self::Storage(_) => "storage_failure",
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
