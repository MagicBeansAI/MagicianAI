//! Local-generation kitty settings: switch `runtime.ollama.local_generation.selected`
//! and reload the generation Ollama daemon.
//!
//! GET reports the kitty, host RAM, the RAM-tier recommendation, install
//! status, and per-model warnings. PUT writes the pin even when those
//! warnings fire, then reloads magician-config and (by default) runs
//! `scripts/run-ollama.sh` so the new model is the one that is resident.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use magician::config::magician_config_path;
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::local_generation_settings::{
    assert_known_model, catalog_path, known_model_ids, load_catalog, load_envelope_from_path,
    model_is_installed, parse_config_view, run_ollama_script_path, write_selected,
    LocalGenerationEnvelope, LocalGenerationError,
};

use crate::web_api::MagicianV2Api;

const OLLAMA_RELOAD_TIMEOUT: Duration = Duration::from_secs(480);

#[derive(Debug, Deserialize)]
pub struct PutLocalGenerationRequest {
    pub selected: String,
    #[serde(default = "default_reload_ollama")]
    pub reload_ollama: bool,
}

fn default_reload_ollama() -> bool {
    true
}

fn io_error_response(error: LocalGenerationError) -> HttpResponse {
    let status = match &error {
        LocalGenerationError::UnknownModel { .. } | LocalGenerationError::InvalidModel(_) => {
            actix_web::http::StatusCode::BAD_REQUEST
        },
        LocalGenerationError::Anchor(_) => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
        LocalGenerationError::Io(_) => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut body = json!({
        "error": match &error {
            LocalGenerationError::UnknownModel { .. } => "unknown_local_generation_model",
            LocalGenerationError::InvalidModel(_) => "invalid_local_generation_model",
            LocalGenerationError::Anchor(_) => "local_generation_anchor_missing",
            LocalGenerationError::Io(_) => "local_generation_io_error",
        },
        "message": error.to_string(),
    });
    if let LocalGenerationError::UnknownModel { known, .. } = &error {
        if let Some(object) = body.as_object_mut() {
            object.insert("known".to_string(), json!(known));
        }
    }
    HttpResponse::build(status).json(body)
}

fn generation_base_url() -> String {
    std::env::var("MAGICIAN_OLLAMA_URL")
        .or_else(|_| std::env::var("MAGICIAN_OLLAMA_BASE_URL"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:11434".to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}

async fn installed_models() -> (HashMap<String, bool>, Option<String>) {
    let url = format!("{}/api/tags", generation_base_url());
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                HashMap::new(),
                Some(format!("could not build Ollama client: {error}")),
            )
        },
    };
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>().await {
                Ok(payload) => {
                    let names: Vec<String> = payload
                        .get("models")
                        .and_then(|value| value.as_array())
                        .map(|models| {
                            models
                                .iter()
                                .filter_map(|model| {
                                    model
                                        .get("name")
                                        .and_then(|name| name.as_str())
                                        .map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut installed = HashMap::new();
                    for name in &names {
                        installed.insert(name.clone(), true);
                        if let Some(stripped) = name.strip_suffix(":latest") {
                            installed.insert(stripped.to_string(), true);
                        }
                    }
                    (installed, None)
                },
                Err(error) => (
                    HashMap::new(),
                    Some(format!("could not parse Ollama /api/tags: {error}")),
                ),
            }
        },
        Ok(response) => (
            HashMap::new(),
            Some(format!(
                "Ollama /api/tags returned HTTP {}",
                response.status()
            )),
        ),
        Err(error) => (
            HashMap::new(),
            Some(format!("Ollama is unreachable at {url}: {error}")),
        ),
    }
}

fn fill_installed(
    mut envelope: LocalGenerationEnvelope,
    installed: &HashMap<String, bool>,
    probe_error: Option<String>,
) -> LocalGenerationEnvelope {
    if let Some(error) = probe_error {
        envelope.warnings.push(error);
        return envelope;
    }
    for model in &mut envelope.models {
        let present = installed
            .get(&model.id)
            .copied()
            .or_else(|| installed.get(&model.ollama).copied())
            .unwrap_or_else(|| {
                model_is_installed(&installed.keys().cloned().collect::<Vec<_>>(), &model.id)
                    || model_is_installed(
                        &installed.keys().cloned().collect::<Vec<_>>(),
                        &model.ollama,
                    )
            });
        if model.installed.is_none() {
            model.installed = Some(present);
        }
        if model.installed == Some(false)
            && !model
                .warnings
                .iter()
                .any(|warning| warning.contains("not installed"))
        {
            model.warnings.push(format!(
                "{} is not installed in Ollama; pin it anyway, then `make setup-local-generation MODEL={}`",
                model.label, model.id
            ));
        }
    }
    envelope
}

async fn reload_ollama_daemon() -> Result<String, String> {
    let script = run_ollama_script_path()
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            "could not find scripts/run-ollama.sh beside the Magician source tree".to_string()
        })?;
    let root = script
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| {
            "run-ollama.sh is not in a scripts/ directory under a Magician checkout".to_string()
        })?;
    let bash = runtime_core::process::resolve_program_str("bash", None);
    let mut command = tokio::process::Command::new(&bash);
    command
        .arg(&script)
        .current_dir(root)
        .env("MAGICIAN_CONFIG_PATH", magician_config_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(OLLAMA_RELOAD_TIMEOUT, command.output())
        .await
        .map_err(|_| "run-ollama.sh timed out after 8 minutes".to_string())?
        .map_err(|error| format!("failed to run run-ollama.sh: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(format!(
            "run-ollama.sh exited {}: {}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// GET /api/magician/v2/settings/local-generation
pub async fn get_local_generation_settings_handler() -> Result<HttpResponse> {
    let path = magician_config_path();
    let (installed, probe_error) = installed_models().await;
    match load_envelope_from_path(&path, &installed) {
        Ok(envelope) => {
            Ok(HttpResponse::Ok().json(fill_installed(envelope, &installed, probe_error)))
        },
        Err(error) => Ok(io_error_response(error)),
    }
}

/// PUT /api/magician/v2/settings/local-generation — durable pin + live reload.
/// RAM-tier violations are returned as warnings; the switch still happens.
pub async fn put_local_generation_settings_handler(
    api: web::Data<MagicianV2Api>,
    agent_resources: web::Data<Arc<AgentResources>>,
    llm_content_settings: Option<
        web::Data<
            magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureSettingsHandle,
        >,
    >,
    body: web::Json<PutLocalGenerationRequest>,
) -> Result<HttpResponse> {
    let selected = body.selected.trim().to_string();
    let reload_ollama = body.reload_ollama;
    let path = magician_config_path();
    let yaml = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => return Ok(io_error_response(LocalGenerationError::Io(error))),
    };
    let snapshot = parse_config_view(&yaml);
    let catalog_file = catalog_path();
    let catalog = catalog_file
        .as_ref()
        .map(|path| load_catalog(path))
        .unwrap_or_default();
    if let Err(error) = assert_known_model(&snapshot, &catalog, &selected) {
        return Ok(io_error_response(error));
    }
    let previous = snapshot.block.selected.clone();
    if let Err(error) = write_selected(&path, &selected) {
        return Ok(io_error_response(error));
    }

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
    let config_reloaded = reload_outcome.is_none();

    let mut ollama_reloaded = false;
    let mut ollama_error = None;
    let mut ollama_log = None;
    if reload_ollama {
        match reload_ollama_daemon().await {
            Ok(log) => {
                ollama_reloaded = true;
                if !log.is_empty() {
                    ollama_log = Some(log);
                }
            },
            Err(error) => ollama_error = Some(error),
        }
    }

    let (installed, probe_error) = installed_models().await;
    let envelope = match load_envelope_from_path(&path, &installed) {
        Ok(envelope) => fill_installed(envelope, &installed, probe_error),
        Err(error) => return Ok(io_error_response(error)),
    };

    let rule_violated = envelope
        .models
        .iter()
        .find(|model| model.id == selected)
        .map(|model| !model.rule_ok)
        .unwrap_or(false);

    let mut body = serde_json::to_value(&envelope).unwrap_or_else(|_| json!({}));
    if let Some(object) = body.as_object_mut() {
        object.insert("previous".to_string(), json!(previous));
        object.insert("config_reloaded".to_string(), json!(config_reloaded));
        object.insert("ollama_reloaded".to_string(), json!(ollama_reloaded));
        object.insert("rule_violated".to_string(), json!(rule_violated));
        object.insert(
            "known".to_string(),
            json!(known_model_ids(&snapshot.block, &catalog)),
        );
        if let Some(error) = reload_outcome {
            object.insert("reload_error".to_string(), json!(error));
        }
        if let Some(error) = ollama_error {
            object.insert("ollama_error".to_string(), json!(error));
        }
        if let Some(log) = ollama_log {
            object.insert("ollama_log".to_string(), json!(log));
        }
    }
    Ok(HttpResponse::Ok().json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_request_defaults_to_reloading_ollama() {
        let request: PutLocalGenerationRequest =
            serde_json::from_str(r#"{"selected":"woof-4b"}"#).expect("parse");
        assert_eq!(request.selected, "woof-4b");
        assert!(request.reload_ollama);
    }

    #[test]
    fn put_request_can_skip_the_daemon_reload() {
        let request: PutLocalGenerationRequest =
            serde_json::from_str(r#"{"selected":"gemma4:12b","reload_ollama":false}"#)
                .expect("parse");
        assert!(!request.reload_ollama);
    }
}
