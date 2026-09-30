//! Realtime provider factory — instantiates a concrete
//! [`RealtimeProvider`] from a [`RealtimeVoiceProfile`].
//!
//! Why this lives in magicllm rather than magician:
//!   - The factory needs to dispatch on `profile.provider` string keys
//!     against the impls we ship here. Keeping the dispatch table
//!     beside the impls means "add a new provider" is one new file
//!     in `magicllm/src/realtime/` plus a single match arm here —
//!     not a cross-crate change.
//!   - magician's bootstrap code stays generic: it asks the router
//!     for an `Arc<dyn RealtimeProvider>` and uses it. No provider
//!     names appear anywhere outside this factory + YAML config.
//!
//! API keys come from env vars (per-provider convention) rather than
//! YAML — we never want secrets in the magician-config.yaml file.

use std::sync::Arc;

use crate::config::RealtimeVoiceProfile;
use crate::realtime::gemini::GeminiLiveProvider;
use crate::realtime::openai::OpenAiRealtimeProvider;
use crate::realtime::provider::RealtimeProvider;
use crate::realtime::types::RealtimeProviderError;

/// Build a [`RealtimeProvider`] from a profile. Reads any required
/// secrets from env (per the provider's documented var name).
/// Returns `NotConfigured` when the env var is missing — the caller
/// (orchestrator) treats this as "feature disabled, fall back to
/// browser-native or error" rather than crashing the process.
pub fn build_realtime_provider(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    match profile.provider.as_str() {
        "openai_realtime" | "openai-realtime" => build_openai(profile),
        "openai_realtime_backend" | "openai-realtime-backend" => build_openai_backend(profile),
        "openai_live" | "openai-live" | "gpt_live" => build_openai_live(profile),
        "gemini_live" | "gemini-live" => build_gemini_live(profile),
        "grok_voice" | "grok-voice" | "xai_voice" => build_grok_voice(profile),
        other => Err(RealtimeProviderError::NotConfigured(format!(
            "unknown realtime provider in profile: {other}"
        ))),
    }
}

fn build_openai(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        RealtimeProviderError::NotConfigured(
            "OPENAI_API_KEY env var is required for the openai_realtime provider".to_string(),
        )
    })?;
    let base_url = profile
        .base_url
        .clone()
        .unwrap_or_else(|| crate::realtime::openai::OPENAI_REALTIME_DEFAULT_BASE_URL.to_string());
    validate_openai_transcription_profile(profile, false)?;
    // Profile.model is non-optional in YAML but a blank string is
    // possible — fall back to the const so an empty value yields a
    // working session instead of a 400 from upstream.
    let model = if profile.model.trim().is_empty() {
        crate::realtime::openai::OPENAI_REALTIME_DEFAULT_MODEL.to_string()
    } else {
        profile.model.clone()
    };
    let provider = OpenAiRealtimeProvider::with_client(
        Arc::new(
            reqwest::Client::builder()
                .build()
                .map_err(|e| RealtimeProviderError::NotConfigured(e.to_string()))?,
        ),
        api_key,
        base_url,
    )
    .with_defaults(model, profile.voice.clone())
    .with_profile_overrides(
        profile.transcription_model.clone(),
        profile.transcription_fallback_model.clone(),
        profile.turn_detection_mode.clone(),
        profile.context_window_tokens,
    );
    Ok(Arc::new(provider))
}

fn build_openai_live(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        RealtimeProviderError::NotConfigured(
            "OPENAI_API_KEY env var is required for the openai_live provider".to_string(),
        )
    })?;
    let websocket_url = profile.base_url.clone().unwrap_or_else(|| {
        crate::realtime::openai_live::OPENAI_LIVE_DEFAULT_WEBSOCKET_URL.to_string()
    });
    let model = if profile.model.trim().is_empty() {
        crate::realtime::openai_live::OPENAI_LIVE_DEFAULT_MODEL.to_string()
    } else {
        profile.model.clone()
    };
    let provider = crate::realtime::openai_live::OpenAiLiveProvider::new(api_key)
        .with_websocket_url(websocket_url)
        .with_defaults(model, profile.voice.clone())
        .with_max_session_duration_secs(profile.max_session_duration_secs);
    Ok(Arc::new(provider))
}

fn build_gemini_live(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| {
        RealtimeProviderError::NotConfigured(
            "GEMINI_API_KEY env var is required for the gemini_live provider".to_string(),
        )
    })?;
    let websocket_url = profile
        .base_url
        .clone()
        .unwrap_or_else(|| crate::realtime::gemini::GEMINI_LIVE_DEFAULT_WEBSOCKET_URL.to_string());
    let model = if profile.model.trim().is_empty() {
        crate::realtime::gemini::GEMINI_LIVE_DEFAULT_MODEL.to_string()
    } else {
        profile.model.clone()
    };
    let (thinking_level, tool_result_scheduling) = validate_gemini_live_options(profile, &model)?;
    let provider = GeminiLiveProvider::new(api_key)
        .with_websocket_url(websocket_url)
        .with_defaults(model, profile.voice.clone())
        .with_profile_overrides(
            profile.turn_detection_mode.clone(),
            profile.context_window_tokens,
            profile.max_session_duration_secs,
        )
        .with_mode(
            profile.mode,
            profile.translation_target_language.clone(),
            profile.translation_echo_target_language,
        )
        .with_live_options(thinking_level, tool_result_scheduling);
    Ok(Arc::new(provider))
}

/// Check a Gemini profile's reasoning/scheduling knobs against the model's
/// published contract. A mismatch is `NotConfigured` so the engine picker
/// lists the profile as unavailable with this reason, instead of the socket
/// closing 1007 on the first call.
fn validate_gemini_live_options(
    profile: &RealtimeVoiceProfile,
    model: &str,
) -> Result<
    (
        Option<crate::realtime::gemini::GeminiThinkingLevel>,
        crate::realtime::gemini::GeminiToolResultScheduling,
    ),
    RealtimeProviderError,
> {
    use crate::realtime::gemini::{
        gemini_live_model_contract, GeminiThinkingLevel, GeminiToolResultScheduling,
    };
    let contract = gemini_live_model_contract(model);
    let thinking_level = match profile.thinking_level.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let level = GeminiThinkingLevel::parse(raw).ok_or_else(|| {
                RealtimeProviderError::NotConfigured(format!(
                    "thinking_level {raw:?} is not one of low, medium, high"
                ))
            })?;
            if !contract.thinking_level {
                return Err(RealtimeProviderError::NotConfigured(format!(
                    "thinking_level is only accepted by gemini-3.8-live-extended-thinking, not \
                     {model}"
                )));
            }
            Some(level)
        },
    };
    let tool_result_scheduling = match profile.tool_result_scheduling.as_deref().map(str::trim) {
        None | Some("") => GeminiToolResultScheduling::default(),
        Some(raw) => {
            let scheduling = GeminiToolResultScheduling::parse(raw).ok_or_else(|| {
                RealtimeProviderError::NotConfigured(format!(
                    "tool_result_scheduling {raw:?} is not one of when_idle, interrupt, silent"
                ))
            })?;
            if !contract.async_function_calling {
                return Err(RealtimeProviderError::NotConfigured(format!(
                    "tool_result_scheduling needs a model with non-blocking function calling \
                     (gemini-3.8-live or newer), not {model}"
                )));
            }
            scheduling
        },
    };
    Ok((thinking_level, tool_result_scheduling))
}

fn build_grok_voice(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    let api_key = std::env::var("XAI_API_KEY").map_err(|_| {
        RealtimeProviderError::NotConfigured(
            "XAI_API_KEY env var is required for the grok_voice provider".to_string(),
        )
    })?;
    let base_url = profile.base_url.clone().unwrap_or_else(|| {
        crate::realtime::openai::GROK_VOICE_DEFAULT_WEBSOCKET_URL.to_string()
    });
    let model = if profile.model.trim().is_empty() {
        crate::realtime::openai::GROK_VOICE_DEFAULT_MODEL.to_string()
    } else {
        profile.model.clone()
    };
    let voice = profile.voice.clone().or_else(|| {
        Some(crate::realtime::openai::GROK_VOICE_DEFAULT_VOICE.to_string())
    });
    let provider = OpenAiRealtimeProvider::with_client(
        Arc::new(
            reqwest::Client::builder()
                .build()
                .map_err(|e| RealtimeProviderError::NotConfigured(e.to_string()))?,
        ),
        api_key,
        base_url,
    )
    .grok_voice()
    .with_defaults(model, voice)
    .with_profile_overrides(
        profile.transcription_model.clone(),
        profile.transcription_fallback_model.clone(),
        profile.turn_detection_mode.clone(),
        profile.context_window_tokens,
    )
    .with_max_session_duration_secs(profile.max_session_duration_secs);
    Ok(Arc::new(provider))
}

fn build_openai_backend(
    profile: &RealtimeVoiceProfile,
) -> Result<Arc<dyn RealtimeProvider>, RealtimeProviderError> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
        RealtimeProviderError::NotConfigured(
            "OPENAI_API_KEY env var is required for the openai_realtime_backend provider"
                .to_string(),
        )
    })?;
    let base_url = profile.base_url.clone().unwrap_or_else(|| {
        crate::realtime::openai::OPENAI_REALTIME_DEFAULT_WEBSOCKET_URL.to_string()
    });
    validate_openai_transcription_profile(profile, true)?;
    let model = if profile.model.trim().is_empty() {
        crate::realtime::openai::OPENAI_REALTIME_DEFAULT_MODEL.to_string()
    } else {
        profile.model.clone()
    };
    let provider = OpenAiRealtimeProvider::with_client(
        Arc::new(
            reqwest::Client::builder()
                .build()
                .map_err(|e| RealtimeProviderError::NotConfigured(e.to_string()))?,
        ),
        api_key,
        base_url,
    )
    .with_defaults(model, profile.voice.clone())
    .backend_proxied()
    .with_profile_overrides(
        profile.transcription_model.clone(),
        profile.transcription_fallback_model.clone(),
        profile.turn_detection_mode.clone(),
        profile.context_window_tokens,
    );
    Ok(Arc::new(provider))
}

fn validate_openai_transcription_profile(
    profile: &RealtimeVoiceProfile,
    backend_proxied: bool,
) -> Result<(), RealtimeProviderError> {
    let model = profile
        .transcription_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            RealtimeProviderError::NotConfigured(
                "OpenAI realtime profile requires transcription_model".to_string(),
            )
        })?;
    if model.eq_ignore_ascii_case("local") {
        if !backend_proxied {
            return Err(RealtimeProviderError::NotConfigured(
                "local realtime transcription requires a backend-proxied profile".to_string(),
            ));
        }
        let fallback = profile
            .transcription_fallback_model
            .as_deref()
            .map(str::trim)
            .filter(|fallback| !fallback.is_empty());
        if fallback.is_none() {
            return Err(RealtimeProviderError::NotConfigured(
                "local realtime transcription requires transcription_fallback_model".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(transcription_model: Option<&str>, fallback: Option<&str>) -> RealtimeVoiceProfile {
        RealtimeVoiceProfile {
            provider: "openai_realtime_backend".to_string(),
            model: "gpt-realtime-test".to_string(),
            display_name: None,
            selectable: false,
            mode: crate::config::RealtimeVoiceMode::Assistant,
            allow_without_turn_grounding: false,
            voice: None,
            max_session_duration_secs: None,
            compaction_token_watermark: None,
            base_url: None,
            fallback: Vec::new(),
            transcription_model: transcription_model.map(str::to_string),
            transcription_fallback_model: fallback.map(str::to_string),
            turn_detection_mode: None,
            context_window_tokens: None,
            verbatim_recent_turns: None,
            compaction_input_turn_limit: None,
            translation_target_language: None,
            translation_echo_target_language: false,
            thinking_level: None,
            tool_result_scheduling: None,
            display_order: None,
        }
    }

    fn gemini_profile(
        model: &str,
        thinking_level: Option<&str>,
        tool_result_scheduling: Option<&str>,
    ) -> RealtimeVoiceProfile {
        RealtimeVoiceProfile {
            provider: "gemini_live".to_string(),
            model: model.to_string(),
            thinking_level: thinking_level.map(str::to_string),
            tool_result_scheduling: tool_result_scheduling.map(str::to_string),
            ..profile(None, None)
        }
    }

    #[test]
    fn gemini_options_are_validated_against_the_model_contract() {
        use crate::realtime::gemini::{GeminiThinkingLevel, GeminiToolResultScheduling};

        let extended = gemini_profile(
            "gemini-3.8-live-extended-thinking",
            Some("medium"),
            Some("interrupt"),
        );
        assert_eq!(
            validate_gemini_live_options(&extended, &extended.model).unwrap(),
            (
                Some(GeminiThinkingLevel::Medium),
                GeminiToolResultScheduling::Interrupt
            )
        );

        // Nothing configured is always valid and lands on the defaults.
        let bare = gemini_profile("gemini-3.1-flash-live-preview", None, Some("  "));
        assert_eq!(
            validate_gemini_live_options(&bare, &bare.model).unwrap(),
            (None, GeminiToolResultScheduling::WhenIdle)
        );

        // 3.8 Live has no level knob: the profile is refused, not silently
        // shipped to a model that closes the socket on the field.
        let level_on_live = gemini_profile("gemini-3.8-live", Some("low"), None);
        let error = validate_gemini_live_options(&level_on_live, &level_on_live.model)
            .unwrap_err()
            .to_string();
        assert!(error.contains("thinking_level"), "{error}");
        assert!(error.contains("gemini-3.8-live"), "{error}");

        // 3.1 blocks on every call, so a scheduling hint is a misconfiguration.
        let scheduling_on_legacy =
            gemini_profile("gemini-3.1-flash-live-preview", None, Some("when_idle"));
        let error =
            validate_gemini_live_options(&scheduling_on_legacy, &scheduling_on_legacy.model)
                .unwrap_err()
                .to_string();
        assert!(error.contains("tool_result_scheduling"), "{error}");

        // Unknown spellings are refused with the accepted set in the message.
        let bad_level = gemini_profile("gemini-3.8-live-extended-thinking", Some("minimal"), None);
        let error = validate_gemini_live_options(&bad_level, &bad_level.model)
            .unwrap_err()
            .to_string();
        assert!(error.contains("low, medium, high"), "{error}");
        let bad_scheduling = gemini_profile("gemini-3.8-live", None, Some("asap"));
        let error = validate_gemini_live_options(&bad_scheduling, &bad_scheduling.model)
            .unwrap_err()
            .to_string();
        assert!(error.contains("when_idle, interrupt, silent"), "{error}");
    }

    #[test]
    fn local_transcription_requires_backend_topology_and_explicit_vendor_fallback() {
        let configured = profile(Some("local"), Some("configured-vendor-stt"));
        assert!(validate_openai_transcription_profile(&configured, true).is_ok());
        assert!(validate_openai_transcription_profile(&configured, false).is_err());
        assert!(
            validate_openai_transcription_profile(&profile(Some("local"), None), true).is_err()
        );
        assert!(validate_openai_transcription_profile(&profile(None, None), true).is_err());
    }
}
