//! The dictation-mode policy (plan workstream 3.5).
//!
//! Extracted verbatim from `media_api`: which STT/TTS provider serves the
//! dictation surface — explicit overrides (with `auto`/`default` treated as
//! unset), the unknown-explicit rejection, the availability gate for
//! explicit picks, and `AudioSurface::Dictation` profile resolution. The
//! rails pieces (provider registry, preferences store, audio runtime) are
//! passed in; `media_api` keeps same-signature delegations so its handlers
//! and pre-existing tests compile unedited.

use std::collections::BTreeMap;

use actix_web::HttpResponse;
use serde_json::json;

use magician_media::media_rails::{
    AudioRuntimeConfigManager, AudioStage, AudioSurface, MediaPreferencesStore,
    MediaProviderRegistry, ProviderAvailability,
};

/// Normalize a caller-supplied explicit provider: trimmed, empty, `auto`,
/// and `default` all mean "unset, use the dictation profile".
pub fn explicit_audio_provider_override(explicit_provider: Option<&str>) -> Option<&str> {
    explicit_provider
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("auto"))
        .filter(|value| !value.eq_ignore_ascii_case("default"))
}

pub async fn resolve_dictation_stt_provider(
    providers: &MediaProviderRegistry,
    preferences: &MediaPreferencesStore,
    audio_runtime: &AudioRuntimeConfigManager,
    principal: &str,
    workspace: &str,
    explicit_provider: Option<&str>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> std::result::Result<Option<String>, HttpResponse> {
    let explicit = explicit_audio_provider_override(explicit_provider);
    if let Some(explicit) = explicit {
        if !providers
            .stt_chain()
            .iter()
            .any(|provider| provider.id().eq_ignore_ascii_case(explicit))
        {
            return Err(HttpResponse::BadRequest().json(json!({
                "error": "unknown_stt_provider",
                "provider": explicit,
            })));
        }
        ensure_explicit_audio_provider_available(
            audio_runtime,
            AudioStage::RecordingStt,
            explicit,
            "stt_provider_unavailable",
        )?;
        return Ok(Some(explicit.to_string()));
    }

    let preferences = preferences
        .load(principal, workspace, audio_runtime)
        .await
        .map_err(media_preferences_error_response)?;
    let resolved = audio_runtime
        .resolve(
            AudioSurface::Dictation,
            &preferences,
            explicit_profile,
            explicit_stage_options,
        )
        .map_err(audio_config_error_response)?;
    resolved
        .stages
        .get(&AudioStage::RecordingStt)
        .and_then(|stage| stage.selected.as_ref())
        .map(|option| Some(option.provider_id.clone()))
        .ok_or_else(|| {
            HttpResponse::ServiceUnavailable().json(json!({
                "error": "stt_provider_not_configured",
                "message": "The resolved Dictation profile has no available recording STT provider.",
            }))
        })
}

pub async fn resolve_dictation_tts_provider(
    providers: &MediaProviderRegistry,
    preferences: &MediaPreferencesStore,
    audio_runtime: &AudioRuntimeConfigManager,
    principal: &str,
    workspace: &str,
    explicit_provider: Option<&str>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> std::result::Result<Option<String>, HttpResponse> {
    let explicit = explicit_audio_provider_override(explicit_provider);
    if let Some(explicit) = explicit {
        if !providers
            .tts_chain()
            .iter()
            .any(|provider| provider.id().eq_ignore_ascii_case(explicit))
        {
            return Err(HttpResponse::BadRequest().json(json!({
                "error": "unknown_tts_provider",
                "provider": explicit,
            })));
        }
        ensure_explicit_audio_provider_available(
            audio_runtime,
            AudioStage::Tts,
            explicit,
            "tts_provider_unavailable",
        )?;
        return Ok(Some(explicit.to_string()));
    }

    let preferences = preferences
        .load(principal, workspace, audio_runtime)
        .await
        .map_err(media_preferences_error_response)?;
    let resolved = audio_runtime
        .resolve(
            AudioSurface::Dictation,
            &preferences,
            explicit_profile,
            explicit_stage_options,
        )
        .map_err(audio_config_error_response)?;
    resolved
        .stages
        .get(&AudioStage::Tts)
        .and_then(|stage| stage.selected.as_ref())
        .map(|option| Some(option.provider_id.clone()))
        .ok_or_else(|| {
            HttpResponse::ServiceUnavailable().json(json!({
                "error": "tts_provider_not_configured",
                "message": "The resolved Dictation profile has no available TTS provider.",
            }))
        })
}

pub fn ensure_explicit_audio_provider_available(
    audio_runtime: &AudioRuntimeConfigManager,
    stage: AudioStage,
    provider_id: &str,
    error_code: &'static str,
) -> std::result::Result<(), HttpResponse> {
    let snapshot = audio_runtime.snapshot();
    let matching = snapshot
        .catalog
        .get(&stage)
        .into_iter()
        .flatten()
        .filter(|option| {
            option.provider_id.eq_ignore_ascii_case(provider_id)
                || option.option_id.eq_ignore_ascii_case(provider_id)
        })
        .collect::<Vec<_>>();
    let Some(option) = matching.first() else {
        return Ok(());
    };
    if matching
        .iter()
        .any(|option| option.availability == ProviderAvailability::Available)
    {
        return Ok(());
    }
    Err(HttpResponse::ServiceUnavailable().json(json!({
        "error": error_code,
        "provider": provider_id,
        "availability": option.availability,
        "message": option.unavailable_reason,
    })))
}

// The two error mappers are the shared media rails-error contract in
// `media_api` (used by the preferences/settings endpoints too); the seam
// reuses them rather than forking the wire shapes.
use crate::media_api::{audio_config_error_response, media_preferences_error_response};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_sentinels_mean_unset() {
        assert_eq!(explicit_audio_provider_override(None), None);
        assert_eq!(explicit_audio_provider_override(Some("")), None);
        assert_eq!(explicit_audio_provider_override(Some("   ")), None);
        assert_eq!(explicit_audio_provider_override(Some("auto")), None);
        assert_eq!(explicit_audio_provider_override(Some("AUTO")), None);
        assert_eq!(explicit_audio_provider_override(Some("default")), None);
        assert_eq!(explicit_audio_provider_override(Some("Default")), None);
        assert_eq!(
            explicit_audio_provider_override(Some(" fluid-qwen ")),
            Some("fluid-qwen")
        );
    }
}
