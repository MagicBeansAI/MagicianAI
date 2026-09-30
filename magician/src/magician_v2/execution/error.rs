use thiserror::Error;

use super::magicutor_client::MagicutorClientError;

/// Error surface shared across execution components.
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("magicutor client error: {0}")]
    Magicutor(#[from] MagicutorClientError),

    #[error("plan is missing executable steps: {0}")]
    InvalidPlan(String),

    #[error("step execution failed: {0}")]
    Step(String),

    /// A deterministic capability returned its declared bounded JSON failure
    /// envelope. Keeping the stable code separate lets application services
    /// route unsupported inputs, auth failures, and retryable failures without
    /// parsing human prose.
    #[error("capability failed ({code}): {message}")]
    CapabilityFailure { code: String, message: String },

    /// Recoverable path/sandbox denial: the requested path(s) fell outside the
    /// currently-allowed file sandbox roots but are otherwise valid targets.
    ///
    /// Distinct from the hard [`ExecutionError::Step`] used for read-only /
    /// delete-policy / repo-fence violations: this variant is meant to be
    /// intercepted at the executor error chokepoint and routed into the
    /// sandbox-override HITL (approve a folder, merge into
    /// `session_file_sandbox_roots`, retry) rather than terminally failing the
    /// iteration. Only the pure out-of-roots case should produce it.
    #[error("path outside allowed file sandbox roots: {}", .paths.join(", "))]
    PathAccessDenied { paths: Vec<String> },

    #[error("invalid execution state: {0}")]
    InvalidState(String),

    #[error("execution persistence error: {0}")]
    Persistence(String),

    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("observation error: {0}")]
    Observation(String),

    #[error("plan has a dependency cycle: {0}")]
    CyclicDependency(String),
}

/// Convenient alias for execution results.
pub type ExecutionResult<T> = std::result::Result<T, ExecutionError>;
