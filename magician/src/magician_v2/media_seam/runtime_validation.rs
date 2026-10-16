//! Runtime-config and downstream-fanout helpers consumed by the lib.

use crate::config::MagicianMediaSettings;
use crate::magician_v2::media_seam::runtime_config::{build_snapshot, prepare_settings};
use crate::magician_v2::media_seam::{
    AudioConfigError, MediaProviderRegistry, VoiceDownstreamMessage,
};

pub fn validate_media_settings(settings: &MagicianMediaSettings) -> Result<(), AudioConfigError> {
    let providers = MediaProviderRegistry::new();
    let (settings, _) = prepare_settings(settings.clone(), &providers);
    build_snapshot(settings, &providers).map(|_| ())
}

/// A run is holding a staged diff and waiting for the user to approve it.
///
/// Unlike `task.completed` this is **not** a lifecycle edge — the state it
/// reports is derived from the `CodeChangeProposal` store and stays true until
/// the user acts, which is why the caller dedupes against a durable log
/// (`artifact_v2::spoken_hitl_log`) rather than relying on the moment of
/// staging. The payload names the proposal so a consumer can correlate it with
/// the `diff_approval` HITL and with the task card's
/// `awaiting_diff_approval`.
pub fn task_awaiting_diff_approval_message(
    task_id: &str,
    title: &str,
    proposal_id: &str,
    changed_file_count: usize,
) -> VoiceDownstreamMessage {
    VoiceDownstreamMessage {
        kind: "task.awaiting_diff_approval".to_string(),
        payload: serde_json::json!({
            "task_id": task_id,
            "title": title,
            "proposal_id": proposal_id,
            "changed_file_count": changed_file_count,
        }),
    }
}
