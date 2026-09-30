use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Stable, credential-safe failures exposed by the MCP boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpClientError {
    #[error("invalid MCP client configuration: {0}")]
    InvalidConfig(String),

    #[error("failed to spawn MCP stdio server at {executable}")]
    Spawn {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("MCP {transport} connection timed out")]
    ConnectTimeout { transport: &'static str },

    #[error("MCP {transport} connection failed: {message}")]
    Connect {
        transport: &'static str,
        message: String,
    },

    #[error("MCP server did not provide negotiated peer information")]
    MissingPeerInfo,

    #[error("MCP client identity allocation is exhausted")]
    ClientIdentityExhausted,

    #[error("MCP discovery timed out")]
    DiscoveryTimeout,

    #[error("MCP discovery failed: {0}")]
    Discovery(String),

    #[error("MCP catalog was rejected: {0}")]
    CatalogRejected(String),

    #[error("MCP tool was not present in the latest validated discovery snapshot")]
    ToolNotDiscovered,

    #[error("MCP tool call timed out")]
    CallTimeout,

    #[error("MCP tool call was cancelled")]
    CallCancelled,

    #[error("MCP tool call failed: {0}")]
    Call(String),

    #[error("MCP request was rejected: {0}")]
    RequestRejected(String),

    #[error("MCP pending continuation capacity is exhausted")]
    ContinuationCapacityExceeded,

    #[error("MCP continuation identity allocation is exhausted")]
    ContinuationIdentityExhausted,

    #[error("MCP continuation state is unavailable")]
    ContinuationStateUnavailable,

    #[error("MCP continuation is not active")]
    ContinuationNotActive,

    #[error("MCP continuation kind does not accept input responses")]
    ContinuationKindMismatch,

    #[error("MCP continuation response contract was rejected")]
    ContinuationResponseRejected,

    #[error("MCP continuation requested an unsupported presentation capability")]
    ContinuationCapabilityUnsupported,

    #[error("MCP continuation round limit was exceeded")]
    ContinuationRoundLimitExceeded,

    #[error("MCP task poll is not ready; retry after {retry_after:?}")]
    TaskPollNotReady { retry_after: Duration },

    #[error("MCP task transport was lost after dispatch")]
    TaskTransportLost,

    #[error("MCP task operation limit was exceeded")]
    TaskOperationLimitExceeded,

    #[error("MCP task is not waiting for input")]
    TaskInputNotRequired,

    #[error("MCP task cancellation was already requested")]
    TaskCancellationAlreadyRequested,

    #[error("MCP task recovery binding was rejected")]
    TaskRecoveryBindingRejected,

    #[error("MCP task recovery record was rejected")]
    TaskRecoveryRecordRejected,

    #[error("MCP recovered task is already active in this client")]
    TaskRecoveryAlreadyActive,

    #[error("MCP continuation is bound to the lost SDK session")]
    ContinuationRecoverySessionBound,

    #[error("MCP notification subscription is unsupported")]
    SubscriptionUnsupported,

    #[error("MCP notification subscription request was rejected")]
    SubscriptionRequestRejected,

    #[error("an MCP notification subscription is already active")]
    SubscriptionAlreadyActive,

    #[error("MCP notification subscription is not active")]
    SubscriptionNotActive,

    #[error("MCP notification subscription identity allocation is exhausted")]
    SubscriptionIdentityExhausted,

    #[error("MCP notification subscription state is unavailable")]
    SubscriptionStateUnavailable,

    #[error("MCP notification subscription timed out")]
    SubscriptionTimeout,

    #[error("MCP notification subscription failed")]
    SubscriptionFailed,

    #[error("MCP notification subscription ended")]
    SubscriptionEnded,

    #[error("MCP notification subscription violated its negotiated contract")]
    SubscriptionProtocolViolation,

    #[error("MCP response was rejected: {0}")]
    ResponseRejected(String),

    #[error("MCP connection shutdown failed: {0}")]
    Shutdown(String),
}
