//! Errors the auth backends report. Everything is a message the product can
//! surface (or the audit trail can record) — no panics across the seam.

use std::fmt;

/// What went wrong talking to an auth backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The endpoint answered, but not with success. `status` is the HTTP
    /// status; `body` is a trimmed excerpt for the operator.
    Http { status: u16, body: String },
    /// The endpoint answered 2xx but the body is not a usable response.
    Protocol(String),
    /// The device code (or session) timed out — the user took too long.
    Expired,
    /// The user actively refused at the authorization endpoint.
    AuthorizationDenied,
    /// The provider's configuration itself is wrong (a missing field, an
    /// unreachable form of URL) — checked before any request goes out.
    Config(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, body } => write!(f, "auth endpoint returned {status}: {body}"),
            Self::Protocol(detail) => write!(f, "auth response not understood: {detail}"),
            Self::Expired => write!(f, "the sign-in window expired; start again"),
            Self::AuthorizationDenied => write!(f, "sign-in was refused by the user"),
            Self::Config(detail) => write!(f, "auth configuration: {detail}"),
        }
    }
}

impl std::error::Error for AuthError {}
