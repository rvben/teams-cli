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
    ConfirmationRequired(String),
    #[error("{0}")]
    Unconfirmed(String),
    #[error("{message} (client message ID: {client_message_id})")]
    UnconfirmedSend {
        message: String,
        chat_id: String,
        client_message_id: String,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Desktop(String),
    #[error("{0}")]
    Unhealthy(String),
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
pub const CONFIRMATION_REQUIRED: ErrorContract = ErrorContract {
    kind: "confirmation_required",
    exit_code: 2,
    retryable: false,
    description: "The operation needs confirmation: answer the prompt in a terminal, or pass --yes",
};
pub const UNCONFIRMED: ErrorContract = ErrorContract {
    kind: "unconfirmed",
    exit_code: 8,
    retryable: false,
    description: "A remote write may have been accepted but was not confirmed; check the conversation before trying again",
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

pub const UNSUPPORTED: ErrorContract = ErrorContract {
    kind: "unsupported",
    exit_code: 2,
    retryable: false,
    description: "The selected backend or platform does not support this operation",
};
pub const DESKTOP: ErrorContract = ErrorContract {
    kind: "desktop_error",
    exit_code: 5,
    retryable: false,
    description: "The desktop backend could not complete the request",
};

pub const UNHEALTHY: ErrorContract = ErrorContract {
    kind: "unhealthy",
    exit_code: 7,
    retryable: false,
    description: "Diagnostics completed and at least one check failed",
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
    CONFIRMATION_REQUIRED,
    UNCONFIRMED,
    UNEXPECTED,
    UNSUPPORTED,
    DESKTOP,
    UNHEALTHY,
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
            Self::ConfirmationRequired(_) => CONFIRMATION_REQUIRED,
            Self::Unconfirmed(_) | Self::UnconfirmedSend { .. } => UNCONFIRMED,
            Self::Unsupported(_) => UNSUPPORTED,
            Self::Desktop(_) => DESKTOP,
            Self::Unhealthy(_) => UNHEALTHY,
            Self::Io(_) | Self::Unexpected(_) => UNEXPECTED,
        }
    }
}

impl AppError {
    /// Appends `hint` to the message while keeping the error kind, so callers can say
    /// what state a failure left behind.
    pub fn with_hint(self, hint: &str) -> Self {
        let add = |message: String| format!("{message} {hint}");
        match self {
            Self::InvalidInput(m) => Self::InvalidInput(add(m)),
            Self::Auth(m) => Self::Auth(add(m)),
            Self::ReadOnly(m) => Self::ReadOnly(add(m)),
            Self::Permission(m) => Self::Permission(add(m)),
            Self::NotFound(m) => Self::NotFound(add(m)),
            Self::Api(m) => Self::Api(add(m)),
            Self::NonInteractive(m) => Self::NonInteractive(add(m)),
            Self::ConfirmationRequired(m) => Self::ConfirmationRequired(add(m)),
            Self::Unconfirmed(m) => Self::Unconfirmed(add(m)),
            Self::UnconfirmedSend {
                message,
                chat_id,
                client_message_id,
            } => Self::UnconfirmedSend {
                message: add(message),
                chat_id,
                client_message_id,
            },
            Self::Unsupported(m) => Self::Unsupported(add(m)),
            Self::Desktop(m) => Self::Desktop(add(m)),
            Self::Unhealthy(m) => Self::Unhealthy(add(m)),
            Self::Unexpected(m) => Self::Unexpected(add(m)),
            Self::Io(err) => Self::Unexpected(add(err.to_string())),
            // The rate-limit message is fixed and carries only the retry delay.
            Self::RateLimit(_) => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_hint_appends_to_the_message_and_keeps_the_kind() {
        let err = AppError::Auth("signed out".into()).with_hint("Port still open.");
        assert_eq!(err.contract().kind, "auth");
        assert_eq!(err.to_string(), "signed out Port still open.");

        let err = AppError::Desktop("no chats".into()).with_hint("Port still open.");
        assert_eq!(err.contract().kind, "desktop_error");
        assert_eq!(err.to_string(), "no chats Port still open.");
    }

    #[test]
    fn with_hint_keeps_io_errors_unexpected() {
        let io = std::io::Error::other("disk full");
        let err = AppError::Io(io).with_hint("Port still open.");
        assert_eq!(err.contract().kind, "unexpected_error");
        assert_eq!(err.to_string(), "disk full Port still open.");
    }

    #[test]
    fn with_hint_leaves_rate_limits_alone() {
        let err = AppError::RateLimit(Some(5)).with_hint("Port still open.");
        assert!(matches!(err, AppError::RateLimit(Some(5))));
    }
}
