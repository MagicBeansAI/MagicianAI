//! Governed client boundary around the official Rust MCP SDK.
//!
//! This crate deliberately does not expose `rmcp` types in its public API. The SDK owns
//! protocol models, framing, lifecycle negotiation, transports, pagination and OAuth
//! mechanics. Callers own scope/auth resolution, authorization, approval, catalog policy,
//! telemetry and artifact handling.

mod catalog_owner;
mod catalog_projection;
mod client;
mod config;
mod continuation;
mod duplex_json_transport;
mod error;
mod http_transport;
mod invalidation;
mod model;
mod mrtr;
mod oauth_coordinator;
mod oauth_persistence;
mod recovery;
mod stdio_transport;
mod task;
mod validation;

pub use catalog_owner::{
    McpCatalogOwnerError, McpCatalogOwnerErrorCode, McpCatalogPublication, McpProductCatalogLimits,
    McpProductCatalogOwner, McpProductCatalogSnapshot, HARD_MAX_PUBLISHED_MCP_ACCOUNTED_BYTES,
    HARD_MAX_PUBLISHED_MCP_SKILLS, HARD_MAX_PUBLISHED_MCP_TOOLS, MCP_PRODUCT_CATALOG_SNAPSHOT_V1,
};
pub use catalog_projection::{
    project_mcp_catalog, McpCatalogProjectionError, McpCatalogProjectionErrorCode,
    McpModelToolDefinition, McpProjectedCatalog, McpProjectedTool, MCP_PROJECTED_CATALOG_V1,
};
pub use client::McpClient;
pub use config::{
    BearerToken, DuplexJsonTransportConfig, McpClientConfig, McpClientLimits,
    McpMrtrPresentationCapabilities, McpSubscriptionCapabilities, McpTaskLifecycleCapabilities,
    McpTransportConfig, StdioTransportConfig, StreamableHttpTransportConfig,
};
pub use continuation::{
    McpClaimedMrtrCall, McpContinuationKind, McpContinuationRevision, McpPendingCall,
    MCP_CONTINUATION_CONTRACT_V1, MCP_MRTR_CLAIM_CONTRACT_V1,
};
pub use error::McpClientError;
pub use invalidation::{
    McpInvalidationState, McpNotificationSubscription, McpSubscriptionRequest,
    MCP_INVALIDATION_CONTRACT_V1,
};
pub use model::{
    McpCallCancellation, McpConnectionInfo, McpToolCallOutcome, McpToolCallResult,
    McpToolDescriptor, McpToolHints, McpToolId,
};
pub use mrtr::{
    McpMrtrInputId, McpMrtrInputKind, McpMrtrInputSlot, McpMrtrResponse, McpPreparedMrtrResponses,
    MCP_MRTR_RESPONSE_CONTRACT_V1,
};
pub use oauth_coordinator::{
    McpOAuthAuthorizationOutcome, McpOAuthAuthorizationStart, McpOAuthCallbackRoute,
    McpOAuthClientIdentity, McpOAuthClientSecret, McpOAuthCoordinator, McpOAuthCoordinatorError,
    McpOAuthCoordinatorErrorCode, McpOAuthCredentialStatus, McpOAuthLifecycleAudit,
    McpOAuthLifecycleOperation, McpOAuthScopeUpgradeStart,
};
pub use oauth_persistence::{
    McpOAuthPersistence, McpOAuthPersistenceError, McpOAuthSecretDocument, McpOAuthVault,
    McpOAuthVaultError, McpOAuthVaultKey, McpOAuthVaultNamespace, McpOAuthVaultWriteMode,
};
pub use recovery::{
    McpContinuationRecoveryDisposition, McpPreparedTaskRecovery, McpTaskRecoveryBinding,
    McpTaskRecoveryCheckpoint, MCP_TASK_RECOVERY_CONTRACT_V1,
};
pub use task::{
    McpPreparedTaskResponses, McpTaskPollOutcome, McpTaskProgress, McpTaskState,
    MCP_TASK_LIFECYCLE_CONTRACT_V1,
};

/// Exact official SDK release selected at this protocol boundary.
pub const RMCP_SDK_VERSION: &str = "3.1.0";

/// Preferred stateless MCP revision.
pub const PREFERRED_PROTOCOL_VERSION: &str = "2026-07-28";

/// Legacy revision accepted while production servers migrate.
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
