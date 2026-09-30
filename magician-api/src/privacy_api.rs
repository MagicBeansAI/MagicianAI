//! Processing-locality settings API: the UI-writable
//! `privacy.processing.mode` switch.
//!
//! GET reports the effective mode and the real per-operation routing it
//! produces; PUT writes the `privacy:` section durably and then triggers the
//! existing config-reload path so the switch is live without a restart.

use std::sync::Arc;

use actix_web::{web, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use magician::config::PrivacyProcessingSettings;
use magician::magician_v2::privacy_processing_settings::PrivacyProcessingSettingsStore;

use crate::web_api::MagicianV2Api;
use magician::magician_v2::execution::agent_resources::AgentResources;

#[derive(Debug, Deserialize)]
pub struct PutPrivacySettingsRequest {
    #[serde(default)]
    pub processing: PrivacyProcessingSettings,
}

fn store() -> PrivacyProcessingSettingsStore {
    // The runtime-root config file is the section this store owns.
    PrivacyProcessingSettingsStore::new(
        magician::config::magician_config_path()
            .parent()
            .unwrap_or(&std::path::PathBuf::from(".")),
    )
}

fn io_error_response(error: std::io::Error) -> HttpResponse {
    HttpResponse::InternalServerError().json(json!({
        "error": "privacy_settings_io_error",
        "message": error.to_string(),
    }))
}

/// GET /api/magician/v2/settings/privacy
pub async fn get_privacy_settings_handler() -> Result<HttpResponse> {
    match store().load_envelope().await {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(io_error_response(error)),
    }
}

/// PUT /api/magician/v2/settings/privacy — durable write + live reload.
pub async fn put_privacy_settings_handler(
    api: web::Data<MagicianV2Api>,
    agent_resources: web::Data<Arc<AgentResources>>,
    llm_content_settings: Option<
        web::Data<
            magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureSettingsHandle,
        >,
    >,
    body: web::Json<PutPrivacySettingsRequest>,
) -> Result<HttpResponse> {
    let settings = body.into_inner().processing;
    let write_store = store();
    // Reject a mode switch that would leave the runtime config durably
    // unloadable BEFORE writing it: derive the mode on the current config
    // and dry-run the full validation pipeline. Without this, `mode: cloud`
    // on a config lacking `app_platform.processing.remote_profile` would
    // pass validation only at boot time — after the section is on disk.
    if settings.mode == magicllm::ProcessingLocality::Cloud {
        let base_path = if write_store.settings_path().exists() {
            write_store.settings_path()
        } else {
            magician::config::magician_config_path()
        };
        match magician::config::load_magician_config_from_path(&base_path) {
            Ok(mut candidate) => {
                candidate.privacy.processing = settings.clone();
                if let Err(error) = magician::config::validate_magician_config_dry_run(candidate) {
                    return Ok(HttpResponse::BadRequest().json(json!({
                        "error": "cloud_mode_precondition_failed",
                        "message": format!(
                            "cloud mode rejected: the config would not load ({error}); \
                             fix the config before switching, e.g. app_platform.processing.\
                             remote_profile must name a declared remote app profile"
                        ),
                    })));
                }
            },
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": "privacy_settings_io_error",
                    "message": format!("current config failed to load for pre-check: {error}"),
                })));
            },
        }
    }
    let envelope = match write_store.save(settings).await {
        Ok(envelope) => envelope,
        Err(error) => return Ok(io_error_response(error)),
    };
    // One switch, live: the section is written durably, now run the same
    // reload the /settings/magician-config/reload endpoint uses so the
    // router (and every locality guard) observes the new mode immediately.
    let reload = api
        .reload_magician_config(
            Some(agent_resources.get_ref().as_ref()),
            llm_content_settings.as_deref().map(Arc::as_ref),
        )
        .await;
    let reload_outcome = match reload {
        Ok(response) if response.status().is_success() => None,
        Ok(response) => Some(format!("reload returned HTTP {}", response.status())),
        Err(error) => Some(format!("reload failed: {error}")),
    };
    let reload_applied = reload_outcome.is_none();
    let mut body = serde_json::to_value(&envelope).unwrap_or_else(|_| json!({}));
    if let Some(object) = body.as_object_mut() {
        object.insert("reload_applied".to_string(), json!(reload_applied));
        if let Some(reload_error) = reload_outcome {
            object.insert("reload_error".to_string(), json!(reload_error));
        }
    }
    if reload_applied {
        Ok(HttpResponse::Ok().json(body))
    } else {
        // The section was written but the live reload refused — surface it
        // as a 500 with the envelope attached so the client knows the mode
        // will apply on the next restart, not now.
        Ok(HttpResponse::InternalServerError().json(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_request_parses_the_section_shape() {
        let request: PutPrivacySettingsRequest =
            serde_json::from_str(r#"{"processing": {"mode": "cloud"}}"#).expect("parse");
        assert_eq!(request.processing.mode, magicllm::ProcessingLocality::Cloud);
    }

    #[test]
    fn put_request_defaults_to_local_when_body_is_empty() {
        let request: PutPrivacySettingsRequest = serde_json::from_str(r#"{}"#).expect("parse");
        assert_eq!(request.processing.mode, magicllm::ProcessingLocality::Local);
    }
}
