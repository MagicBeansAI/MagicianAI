// Error types for Magician service

use thiserror::Error;

/// Result type for Magician operations
pub type Result<T> = std::result::Result<T, MagicianError>;

/// Main error type for Magician service
#[derive(Debug, Error)]
pub enum MagicianError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Service error: {0}")]
    Service(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl MagicianError {
    /// Create a configuration error
    pub fn config(msg: impl Into<String>) -> Self {
        MagicianError::Config(msg.into())
    }

    /// Create a service error
    pub fn service(msg: impl Into<String>) -> Self {
        MagicianError::Service(msg.into())
    }

    /// Create an internal error
    pub fn internal(msg: impl Into<String>) -> Self {
        MagicianError::Internal(msg.into())
    }
}
