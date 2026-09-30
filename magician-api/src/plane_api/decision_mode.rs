//! Owner control for the host's Decision Engine routing policy.
use actix_web::{web, HttpRequest, HttpResponse};
use magician::config::{load_magician_config_from_path, DecisionHostConfig, DecisionMode};
use magician::magician_v2::{
    auth::middleware::AuthRuntime,
    decision_host,
    runtime_settings::{
        config_file_lock, replace_top_level_yaml_block, runtime_settings_paths,
        write_text_file_atomic,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeUpdate {
    mode: DecisionMode,
}

/// Read, modify and persist under the same file lock as other settings writers.
/// Install before releasing it so concurrent mode updates cannot reorder the
/// live policy relative to the file. A failed write never changes the live mode.
fn save_mode(
    path: &Path,
    mode: DecisionMode,
    install: impl FnOnce(&DecisionHostConfig),
) -> anyhow::Result<DecisionHostConfig> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    let mut config = load_magician_config_from_path(path)?;
    config.decision.mode = mode;
    let original = std::fs::read_to_string(path)?;
    let body = serde_yaml::to_string(&config.decision)?;
    let block = format!(
        "decision:\n{}\n",
        body.lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let next = replace_top_level_yaml_block(&original, "decision", &block);
    write_text_file_atomic(path, &next, None)?;
    install(&config.decision);
    Ok(config.decision)
}

pub async fn plane_decision_mode_put_handler(
    req: HttpRequest,
    body: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = super::owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        return response;
    }
    let update = match serde_json::from_value::<ModeUpdate>(body.into_inner()) {
        Ok(update) => update,
        Err(_) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "invalid_decision_mode",
                "message": "Choose all_engines, magician_only, or off."
            }))
        },
    };
    let path = runtime_settings_paths().config_path;
    match tokio::task::spawn_blocking(move || {
        save_mode(&path, update.mode, decision_host::configure)
    })
    .await
    {
        Ok(Ok(config)) => HttpResponse::Ok().json(json!({"decision_mode": config.mode})),
        result => {
            tracing::error!(?result, "Could not save Decision Engine mode");
            HttpResponse::InternalServerError().json(json!({
                "error": "decision_mode_not_saved",
                "message": "Could not save the Decision Engine setting."
            }))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_rail_setting_preserves_transport_and_other_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        let mut config: magician::config::MagicianConfig =
            serde_yaml::from_str(&magician::config::shipped_repo_config_yaml()).unwrap();
        config.decision.socket = Some("/tmp/custom-decision.sock".into());
        config.decision.timeout_ms = 3210;
        config.execution.harness_engine = "pi".into();
        // Full-config serialization deliberately omits router tables. Keep
        // the composed seed intact so this fixture is self-contained.
        let contents = replace_top_level_yaml_block(
            &magician::config::shipped_repo_config_yaml(),
            "decision",
            "decision:\n  mode: all_engines\n  socket: /tmp/custom-decision.sock\n  timeout_ms: 3210\n",
        );
        let contents = replace_top_level_yaml_block(
            &contents,
            "execution",
            &super::super::render_execution_config_block(&config.execution),
        );
        std::fs::write(&path, contents).unwrap();
        for mode in [
            DecisionMode::Off,
            DecisionMode::MagicianOnly,
            DecisionMode::AllEngines,
        ] {
            let saved = save_mode(&path, mode, |installed| {
                let persisted = load_magician_config_from_path(&path).unwrap();
                assert_eq!(installed, &persisted.decision);
            })
            .unwrap();
            assert_eq!(saved.mode, mode);
            assert_eq!(saved.socket, config.decision.socket);
            assert_eq!(saved.timeout_ms, 3210);
            assert_eq!(
                load_magician_config_from_path(&path)
                    .unwrap()
                    .execution
                    .harness_engine,
                "pi"
            );
        }
        assert!(save_mode(
            &dir.path().join("missing.yaml"),
            DecisionMode::Off,
            |_| panic!("must not install on failure")
        )
        .is_err());
    }

    #[test]
    fn decision_rail_setting_rejects_unknown_or_missing_mode() {
        for body in [
            json!({}),
            json!({"mode":"enabled"}),
            json!({"mode":true}),
            json!({"mode":"off", "enabled":true}),
        ] {
            assert!(serde_json::from_value::<ModeUpdate>(body).is_err());
        }
    }

    #[actix_web::test]
    async fn decision_rail_setting_refuses_an_unauthenticated_caller() {
        let req = actix_web::test::TestRequest::put().to_http_request();
        let response =
            plane_decision_mode_put_handler(req, web::Json(json!({"mode":"off"})), None).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
    }
}
