use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    ReadOnly(String),
    #[error("{0}")]
    Permission(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Microsoft Graph throttled the request")]
    RateLimit(Option<u64>),
    #[error("{0}")]
    Api(String),
    #[error("{0}")]
    NonInteractive(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Unexpected(String),
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ErrorContract {
    pub kind: &'static str,
    pub exit_code: i32,
    pub retryable: bool,
    pub description: &'static str,
}

pub const INVALID_INPUT: ErrorContract = ErrorContract {
    kind: "invalid_input",
    exit_code: 2,
    retryable: false,
    description: "Arguments or local configuration are invalid",
};
pub const AUTH: ErrorContract = ErrorContract {
    kind: "auth",
    exit_code: 3,
    retryable: false,
    description: "Authentication is missing, expired, or rejected",
};
pub const PERMISSION: ErrorContract = ErrorContract {
    kind: "permission_denied",
    exit_code: 5,
    retryable: false,
    description: "The tenant or granted scopes do not permit the operation",
};
pub const NOT_FOUND: ErrorContract = ErrorContract {
    kind: "not_found",
    exit_code: 4,
    retryable: false,
    description: "The requested Teams resource does not exist",
};
pub const RATE_LIMIT: ErrorContract = ErrorContract {
    kind: "rate_limit",
    exit_code: 6,
    retryable: true,
    description: "Microsoft Graph throttled the request",
};
pub const API: ErrorContract = ErrorContract {
    kind: "api_error",
    exit_code: 5,
    retryable: false,
    description: "Microsoft Graph returned an API error",
};
pub const NON_INTERACTIVE: ErrorContract = ErrorContract {
    kind: "tty_required",
    exit_code: 2,
    retryable: false,
    description: "An interactive command was invoked without a terminal",
};
pub const UNEXPECTED: ErrorContract = ErrorContract {
    kind: "unexpected_error",
    exit_code: 1,
    retryable: false,
    description: "An unexpected local or transport error occurred",
};
pub const READ_ONLY: ErrorContract = ErrorContract {
    kind: "read_only",
    exit_code: 2,
    retryable: false,
    description: "The active profile blocks remote write operations",
};

pub const ALL: &[ErrorContract] = &[
    INVALID_INPUT,
    AUTH,
    READ_ONLY,
    PERMISSION,
    NOT_FOUND,
    RATE_LIMIT,
    API,
    NON_INTERACTIVE,
    UNEXPECTED,
];

impl AppError {
    pub fn contract(&self) -> ErrorContract {
        match self {
            Self::InvalidInput(_) => INVALID_INPUT,
            Self::Auth(_) => AUTH,
            Self::ReadOnly(_) => READ_ONLY,
            Self::Permission(_) => PERMISSION,
            Self::NotFound(_) => NOT_FOUND,
            Self::RateLimit(_) => RATE_LIMIT,
            Self::Api(_) => API,
            Self::NonInteractive(_) => NON_INTERACTIVE,
            Self::Io(_) | Self::Unexpected(_) => UNEXPECTED,
        }
    }
}
