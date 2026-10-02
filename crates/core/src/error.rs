use serde::Serialize;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The one error type used across Ancilo.
///
/// Every variant carries a human-readable message that is safe to show to users
/// (no secrets). The [`Error::code`] is stable and machine-readable; API clients
/// and tests match on it.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    PermissionDenied(String),
    /// A consequential operation was invoked without explicit confirmation.
    #[error("{0}")]
    ConfirmationRequired(String),
    /// Not enough RAM or disk for the requested action.
    #[error("{0}")]
    InsufficientResources(String),
    /// A dependency (network, model process, …) is not reachable right now.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::InvalidInput(_) => "invalid_input",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::Unauthorized(_) => "unauthorized",
            Error::PermissionDenied(_) => "permission_denied",
            Error::ConfirmationRequired(_) => "confirmation_required",
            Error::InsufficientResources(_) => "insufficient_resources",
            Error::Unavailable(_) => "unavailable",
            Error::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Error::Internal(e.to_string())
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::InvalidInput(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn unavailable(msg: impl Into<String>) -> Self {
        Error::Unavailable(msg.into())
    }

    /// Wire representation used by every API surface.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: ErrorDetail {
                code: self.code().to_string(),
                message: self.message(),
            },
        }
    }

    /// Reconstructs an error from its wire representation.
    pub fn from_code(code: &str, message: String) -> Self {
        match code {
            "invalid_input" => Error::InvalidInput(message),
            "not_found" => Error::NotFound(message),
            "conflict" => Error::Conflict(message),
            "unauthorized" => Error::Unauthorized(message),
            "permission_denied" => Error::PermissionDenied(message),
            "confirmation_required" => Error::ConfirmationRequired(message),
            "insufficient_resources" => Error::InsufficientResources(message),
            "unavailable" => Error::Unavailable(message),
            _ => Error::Internal(message),
        }
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Internal(format!("I/O error: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("JSON error: {e}"))
    }
}
