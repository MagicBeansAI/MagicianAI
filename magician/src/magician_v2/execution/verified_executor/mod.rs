//! Action Candidate System
//!
//! This module implements action candidate metadata and action-risk support used
//! by the visible agent runtime.
//!
//! ## Design Philosophy: Visible Agent Loop
//!
//! The agent loop chooses actions directly. Browser work is delegated to the
//! `agent-browser` inner loop, where tool results, screenshots/artifacts,
//! runtime ledger updates, and terminal LLM decisions are the source of truth.
//! This module does not run hidden DOM/visual/impact verifiers.
//!
//! ## Key Components
//!
//! - **ActionCandidate**: A ranked action with LLM-provided metadata
//! - **CandidateBatch**: A small ordered set of candidate actions
//! - **CriticalityLevel**: LLM-provided action risk used for confirmation gates
//!
//! ## LLM-Provided Hints
//!
//! | Field | Purpose | When Absent |
//! |-------|---------|-------------|
//! | `criticality_hint` | Internal action risk level | Defaults to Medium |
//! | `is_non_idempotent` | Cannot be safely retried | Assumed false |
//! | `page_context_hint` | Page type for elevation | No elevation applied |
//! | `requires_confirmation` | Must pause for human | False |
//!
//! ## Module Structure
//!
//! ```text
//! verified_executor/
//! ├── mod.rs              - Module exports
//! ├── constants.rs        - Stability constants
//! ├── types.rs            - ActionCandidate, CandidateBatch
//! └── state/              - State detection and stability
//!     ├── mod.rs          - Module exports
//!     ├── fingerprint.rs  - State hashing for stability
//!     └── stability.rs    - Page stability detection
//! ```
//!
//! Note: LLM response parsing is handled by the agentic decision layer, not in
//! this module.
//!
//! ## Usage Example
//!
//! ```rust,ignore
//! use verified_executor::{ActionCandidate, CandidateBatch};
//!
//! // Parse LLM response into candidates
//! let candidates = parse_candidates(llm_response)?;
//! let batch = CandidateBatch::new(candidates, thinking);
//!
//! // Carry one selected action through the visible runtime loop.
//! // Ambiguous failures return to the visible agent loop.
//! ```

pub mod constants;
pub mod executor;
pub mod state;
pub mod types;

// Re-export primary types for convenience
pub use constants::*;
pub use executor::{CriticalityEvaluator, CriticalityLevel};
pub use state::{
    check_immediate_stability, compute_state_hash, wait_for_stability,
    wait_for_stability_progressive, wait_for_stability_with_config, StabilityResult,
};
pub use types::{ActionCandidate, CandidateBatch};

use tool_runtime_core::{
    credential_preparation::CredentialSecretReference,
    credential_profiles::{CredentialProviderId, CredentialScope},
};

use crate::magician_v2::secrets::credential_material_adapter::CredentialGrantRoute;

/// Bind exact metadata after the verified executor admits a delegated-credential use.
/// The returned capability is move-only and must be consumed by sealed preparation.
#[allow(
    dead_code,
    reason = "Phase 5F3 admission remains dormant until governed production routing"
)]
pub fn admit_delegated_credential(
    scope: CredentialScope,
    secret_ref: CredentialSecretReference,
    route: CredentialGrantRoute,
    provider: CredentialProviderId,
    agent_id: String,
) -> types::DelegatedCredentialAdmission {
    types::DelegatedCredentialAdmission::new(scope, secret_ref, route, provider, agent_id)
}
