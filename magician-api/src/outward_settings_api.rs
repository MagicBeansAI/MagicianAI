//! Owner-only, persisted and immediately applied outward sending controls.

use std::path::Path;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

use magician::{
    config::{OutwardActionsConfig, OutwardContextMode, OutwardContextsConfig},
    magician_v2::{
        agents::{
            outward_policy::{self, OutwardOrigin},
            outward_review,
        },
        auth::middleware::AuthRuntime,
        runtime_settings::{
            apply_yaml_section, config_file_lock, runtime_settings_paths, write_text_file_atomic,
        },
    },
};

/// Complete settings, not a partial patch: omitted fields must not reset a
/// browser guard or another context's policy when a stale client saves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutwardSettings {
    pub capture_only: bool,
    pub force_capture_only: bool,
    pub contexts: OutwardContextSettings,
    pub browser_submission_guard: bool,
    /// Ask the owner to review a captured send instead of only recording it.
    /// Defaults on when a client predating it omits it: the omission then
    /// asks, never sends.
    #[serde(default = "review_by_default")]
    pub review_captured_sends: bool,
}

fn review_by_default() -> bool {
    true
}

/// Unlike the backwards-compatible config file, an owner save must name every
/// context. An omitted OPC restriction must never silently become `inherit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutwardContextSettings {
    pub owner_chat: OutwardContextMode,
    pub owner_tasks: OutwardContextMode,
    pub opc: OutwardContextMode,
    /// Envoy and other public-facing agents, split out of OPC.
    pub public_agents: OutwardContextMode,
}

impl From<OutwardActionsConfig> for OutwardSettings {
    fn from(config: OutwardActionsConfig) -> Self {
        Self {
            capture_only: config.capture_only,
            force_capture_only: config.force_capture_only,
            contexts: OutwardContextSettings {
                owner_chat: config.contexts.owner_chat,
                owner_tasks: config.contexts.owner_tasks,
                opc: config.contexts.opc,
                public_agents: config.contexts.public_agents,
            },
            browser_submission_guard: config.browser_submission_guard,
            review_captured_sends: config.review_captured_sends,
        }
    }
}

impl From<OutwardSettings> for OutwardActionsConfig {
    fn from(settings: OutwardSettings) -> Self {
        Self {
            capture_only: settings.capture_only,
            force_capture_only: settings.force_capture_only,
            contexts: OutwardContextsConfig {
                owner_chat: settings.contexts.owner_chat,
                owner_tasks: settings.contexts.owner_tasks,
                opc: settings.contexts.opc,
                public_agents: settings.contexts.public_agents,
            },
            browser_submission_guard: settings.browser_submission_guard,
            review_captured_sends: settings.review_captured_sends,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateOutwardSettings {
    pub expected_revision: String,
    pub settings: OutwardSettings,
}

fn settings_in(contents: &str) -> anyhow::Result<OutwardSettings> {
    let root: serde_yaml::Value = serde_yaml::from_str(contents)?;
    anyhow::ensure!(root.is_mapping(), "configuration root must be a mapping");
    let section = root.get("outward_actions").cloned();
    let config = match section {
        Some(section) => serde_yaml::from_value(section)?,
        None => OutwardActionsConfig::default(),
    };
    Ok(OutwardSettings::from(config))
}

fn revision(settings: &OutwardSettings) -> String {
    blake3::hash(&serde_json::to_vec(settings).expect("outward settings serialize"))
        .to_hex()
        .to_string()
}

fn envelope(settings: OutwardSettings) -> serde_json::Value {
    let current = outward_policy::settings();
    let live = current.clone().unwrap_or_default();
    json!({
        "revision": revision(&settings),
        "applied": current.is_some() && OutwardSettings::from(live.clone()) == settings,
        "effective": {
            "owner_chat": outward_policy::capture_for(&live, OutwardOrigin::OwnerChat),
            "owner_tasks": outward_policy::capture_for(&live, OutwardOrigin::OwnerTask),
            "opc": outward_policy::capture_for(&live, OutwardOrigin::Opc),
            "public_agents": outward_policy::capture_for(&live, OutwardOrigin::PublicAgent),
            "unclassified": outward_policy::capture_for(&live, OutwardOrigin::Unclassified),
        },
        "settings": settings,
    })
}

#[derive(Debug)]
enum SaveError {
    Conflict,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for SaveError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

fn save(
    path: &Path,
    expected_revision: &str,
    settings: &OutwardSettings,
    publish: impl FnOnce(OutwardActionsConfig),
) -> Result<(), SaveError> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path).map_err(anyhow::Error::from)?;
    let before = settings_in(&original)?;
    if revision(&before) != expected_revision {
        return Err(SaveError::Conflict);
    }
    let next = apply_yaml_section(&original, &["outward_actions"], &before, settings)
        .map_err(anyhow::Error::from)?;
    if settings_in(&next)? != *settings {
        return Err(SaveError::Failed(anyhow::anyhow!(
            "outward settings did not round-trip"
        )));
    }
    if next != original {
        write_text_file_atomic(path, &next, None).map_err(anyhow::Error::from)?;
    }
    // No publication before a successful durable write. The config file lock
    // covers publication too, so two settings writers cannot reverse order.
    publish(settings.clone().into());
    Ok(())
}

pub async fn get_handler(req: HttpRequest, auth: Option<web::Data<AuthRuntime>>) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return response;
    }
    let path = runtime_settings_paths().config_path;
    match tokio::task::spawn_blocking(move || -> anyhow::Result<OutwardSettings> {
        let lock = config_file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        settings_in(&std::fs::read_to_string(path)?)
    })
    .await
    {
        Ok(Ok(settings)) => HttpResponse::Ok().json(envelope(settings)),
        error => {
            tracing::error!(?error, "outward settings read failed");
            HttpResponse::InternalServerError().json(json!({"error": "outward_settings_unavailable", "message": "Could not load outward settings."}))
        },
    }
}

pub async fn put_handler(
    req: HttpRequest,
    payload: web::Json<UpdateOutwardSettings>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return response;
    }
    let update = payload.into_inner();
    let path = runtime_settings_paths().config_path;
    let settings = update.settings;
    let saved = settings.clone();
    match tokio::task::spawn_blocking(move || save(&path, &update.expected_revision, &saved, outward_policy::configure)).await {
        Ok(Ok(())) => {
            tracing::warn!(settings = ?settings, "[OUTWARD-POLICY] owner saved and applied sending controls");
            HttpResponse::Ok().json(envelope(settings))
        },
        Ok(Err(SaveError::Conflict)) => HttpResponse::Conflict().json(json!({"error": "outward_settings_changed", "message": "These settings changed elsewhere. Reload before saving."})),
        error => {
            if let Ok(Err(SaveError::Failed(ref reason))) = error { tracing::error!(%reason, "outward settings save failed"); }
            HttpResponse::InternalServerError().json(json!({"error": "outward_settings_not_saved", "message": "Could not save outward settings. The running policy was not changed."}))
        },
    }
}

fn review_grants_file(
    api: &crate::web_api::MagicianV2Api,
    req: &HttpRequest,
) -> Result<std::path::PathBuf, HttpResponse> {
    let scope = crate::scope::resolve_required_scope_ref(req.headers(), None)?;
    let Some(service) = api.orchestrator().artifact_v2_service() else {
        return Err(HttpResponse::ServiceUnavailable().json(json!({
            "error": "task_store_unavailable",
            "message": "The task store is not available.",
        })));
    };
    Ok(outward_review::grants_path(
        service.workspace(),
        &scope.principal(),
        &scope.workspace(),
    ))
}

/// `GET /api/magician/v2/settings/outward-actions/grants` — the owner's
/// "always allow this agent to send to this recipient" grants, newest first.
pub async fn grants_get_handler(
    api: web::Data<crate::web_api::MagicianV2Api>,
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return response;
    }
    let path = match review_grants_file(&api, &req) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let mut grants = outward_review::list(&path);
    grants.sort_by_key(|grant| std::cmp::Reverse(grant.granted_at_ms));
    HttpResponse::Ok().json(json!({ "grants": grants }))
}

/// `DELETE /api/magician/v2/settings/outward-actions/grants/{id}` — revoke
/// one grant; that agent's next send to that recipient is reviewed again.
pub async fn grants_delete_handler(
    api: web::Data<crate::web_api::MagicianV2Api>,
    req: HttpRequest,
    id: web::Path<String>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return response;
    }
    let path = match review_grants_file(&api, &req) {
        Ok(path) => path,
        Err(response) => return response,
    };
    match outward_review::revoke(&path, id.trim()) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound()
            .json(json!({"error": "grant_not_found", "message": "No such grant."})),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": "revoke_failed",
            "message": error.to_string(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::config::OutwardContextMode;

    #[test]
    fn save_preserves_other_config_and_publishes_only_after_durable_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        let original = "# keep\nexecution:\n  god_mode: {enabled: false}\noutward_actions:\n  capture_only: true # baseline\n  browser_submission_guard: true\n";
        std::fs::write(&path, original).unwrap();
        let before = settings_in(original).unwrap();
        let mut next = before.clone();
        next.contexts.owner_chat = OutwardContextMode::Live;
        let published = std::cell::Cell::new(false);
        save(&path, &revision(&before), &next, |live| {
            assert_eq!(
                settings_in(&std::fs::read_to_string(&path).unwrap()).unwrap(),
                OutwardSettings::from(live)
            );
            published.set(true);
        })
        .unwrap();
        assert!(published.get());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("# keep"));
        assert!(contents.contains("god_mode: {enabled: false}"));
        assert!(settings_in(&contents).unwrap().browser_submission_guard);
        assert!(matches!(
            save(&path, &revision(&before), &before, |_| panic!(
                "stale write must not publish"
            )),
            Err(SaveError::Conflict)
        ));
        let missing = tmp.path().join("missing/config.yaml");
        assert!(save(&missing, &revision(&before), &next, |_| panic!(
            "failed write must not publish"
        ))
        .is_err());
    }

    #[test]
    fn partial_or_unknown_settings_cannot_reset_other_controls() {
        assert!(serde_json::from_value::<UpdateOutwardSettings>(
            json!({"expected_revision": "r", "settings": {"capture_only": false}})
        )
        .is_err());
        let mut value =
            serde_json::to_value(OutwardSettings::from(OutwardActionsConfig::default())).unwrap();
        value["allow_all"] = json!(true);
        assert!(serde_json::from_value::<OutwardSettings>(value).is_err());
        let complete =
            serde_json::to_value(OutwardSettings::from(OutwardActionsConfig::default())).unwrap();
        for omitted in ["owner_chat", "owner_tasks", "opc", "public_agents"] {
            let mut partial = complete.clone();
            partial["contexts"].as_object_mut().unwrap().remove(omitted);
            assert!(serde_json::from_value::<OutwardSettings>(partial).is_err());
        }
        // A client that predates review omits it, and the omission asks.
        let mut older = complete.clone();
        older
            .as_object_mut()
            .unwrap()
            .remove("review_captured_sends");
        assert!(
            serde_json::from_value::<OutwardSettings>(older)
                .unwrap()
                .review_captured_sends
        );
    }

    #[actix_web::test]
    async fn unproved_requests_cannot_read_or_write_controls() {
        let request = actix_web::test::TestRequest::default().to_http_request();
        assert!(!get_handler(request, None).await.status().is_success());
        let request = actix_web::test::TestRequest::default().to_http_request();
        let update = UpdateOutwardSettings {
            expected_revision: "r".into(),
            settings: OutwardActionsConfig::default().into(),
        };
        assert!(!put_handler(request, web::Json(update), None)
            .await
            .status()
            .is_success());
    }
}
