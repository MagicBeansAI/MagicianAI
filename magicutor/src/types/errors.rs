use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Bridge error: {0}")]
    BridgeError(String),

    #[error("Bridge timeout while waiting for `{action}` after {timeout_secs} seconds")]
    BridgeTimeout { action: String, timeout_secs: u64 },
}

pub type Result<T> = std::result::Result<T, ExecutionError>;
