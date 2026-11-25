//! Error type shared by [`crate::ops`], [`crate::scan`] and friends.

use redis::RedisError;

/// An error with a user-presentable message.
///
/// `message` is what the original Medis would show in an alert (e.g. the Redis error text such as
/// `WRONGTYPE Operation against a key holding the wrong kind of value`, or a fixed string such as
/// `The key already exists`).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct CoreError {
    /// Human readable message.
    pub message: String,
    /// `true` when the failure indicates that the connection is gone (I/O error, reset, ...).
    pub connection_lost: bool,
}

impl CoreError {
    /// Creates an error from a plain message (never flagged as a lost connection).
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            connection_lost: false,
        }
    }
}

/// Formats a [`RedisError`] the way ioredis would (`<CODE> <detail>`) when it is a server error.
pub fn redis_error_message(e: &RedisError) -> String {
    match (e.code(), e.detail()) {
        (Some(code), Some(detail)) => format!("{code} {detail}"),
        (Some(code), None) => code.to_string(),
        _ => e.to_string(),
    }
}

impl From<RedisError> for CoreError {
    fn from(e: RedisError) -> Self {
        Self {
            message: redis_error_message(&e),
            connection_lost: e.is_connection_dropped(),
        }
    }
}

impl From<redis::ParsingError> for CoreError {
    fn from(e: redis::ParsingError) -> Self {
        Self::msg(e.to_string())
    }
}

impl From<String> for CoreError {
    fn from(s: String) -> Self {
        Self::msg(s)
    }
}

impl From<&str> for CoreError {
    fn from(s: &str) -> Self {
        Self::msg(s)
    }
}

impl From<CoreError> for String {
    fn from(e: CoreError) -> Self {
        e.message
    }
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, CoreError>;
