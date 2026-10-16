//! Trusted native signing boundary for app-proposed memory.
//!
//! The webview may render an already sealed review, but it cannot choose a
//! signing key or arbitrary bytes. This command reconstructs the closed
//! decision document and signs it with the same Keychain identity that the
//! runtime pinned during code-verified desktop pairing.

use magician_app_contract::contribution::{
    AppMemoryOwnerDecisionEnvelopeV1, AppMemoryOwnerDecisionV1, AppMemoryOwnerReviewV1,
};

#[tauri::command]
pub(crate) async fn sign_app_memory_owner_decision(
    review: AppMemoryOwnerReviewV1,
    decision: AppMemoryOwnerDecisionV1,
    retained_until_ms: Option<i64>,
    expected_display_digest: String,
) -> Result<AppMemoryOwnerDecisionEnvelopeV1, String> {
    review
        .validate()
        .map_err(|_| "the app-memory review is invalid".to_owned())?;
    if expected_display_digest != review.display_digest {
        return Err(
            "the confirmed app-memory review digest does not match the complete display".to_owned(),
        );
    }
    crate::app_macos_identity::sign_app_memory_owner_decision(review, decision, retained_until_ms)
}
