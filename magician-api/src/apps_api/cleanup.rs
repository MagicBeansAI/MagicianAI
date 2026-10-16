//! Host-owner data maintenance. Deliberately absent from the app bridge and
//! supported-public query/mutation capability surface.
use super::*;
use magician_apps::apps::entity_retention::{
    AppDataCleanupRequest, AppEntityRetentionService, AppEntityRetentionStoreError,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Confirm,
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRequest {
    operation: Operation,
    #[serde(default)]
    preview_digest: Option<AppDigest>,
    #[serde(default)]
    confirmation: Option<String>,
}

impl ValidateAppContract for ControlRequest {
    fn validate_app_contract(&self, _: &AppContractLimits) -> Result<(), AppContractError> {
        if matches!(self.operation, Operation::Confirm)
            && (self.preview_digest.is_none()
                || self.confirmation.as_deref() != Some("delete_older_app_data"))
        {
            return Err(AppContractError::invalid(
                "confirmation",
                "confirm the reviewed older-data deletion",
            ));
        }
        Ok(())
    }
}

pub(super) fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/installations/{installation_id}/data-cleanup",
        web::get().to(options),
    )
    .route(
        "/installations/{installation_id}/data-cleanup/preview",
        web::post().to(preview),
    )
    .route(
        "/installations/{installation_id}/data-cleanup/{job_ref}/control",
        web::post().to(control),
    )
    .route(
        "/installations/{installation_id}/data-cleanup/{job_ref}/advance",
        web::post().to(advance),
    );
}

async fn options(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let service = AppEntityRetentionService::new(api.registry.clone());
    let entities = match service.cleanup_entities(&authenticated, &id, now).await {
        Ok(value) => value,
        Err(error) => return error_response(error),
    };
    match service.latest_age_cleanup(&authenticated, &id, now).await {
        Ok(latest_job) => no_store(
            serde_json::json!({"installation_id":id,"entities":entities,"latest_job":latest_job}),
        ),
        Err(error) => error_response(error),
    }
}

async fn preview(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request = match read_bounded_app_contract::<AppDataCleanupRequest>(&req, &mut payload).await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    match AppEntityRetentionService::new(api.registry.clone())
        .preview_age_cleanup(&authenticated, &id, request, now)
        .await
    {
        Ok(report) => no_store(report),
        Err(error) => error_response(error),
    }
}

async fn control(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let (id, job) = path.into_inner();
    let id = match AppInstallationId::parse(id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let job = match AppReference::parse(job) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request = match read_bounded_app_contract::<ControlRequest>(&req, &mut payload).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let operation = match request.operation {
        Operation::Confirm => "confirm",
        Operation::Pause => "pause",
        Operation::Resume => "resume",
        Operation::Cancel => "cancel",
    };
    match AppEntityRetentionService::new(api.registry.clone())
        .control_age_cleanup(
            &authenticated,
            &id,
            job,
            operation.to_string(),
            request.preview_digest,
            now,
        )
        .await
    {
        Ok(report) => no_store(report),
        Err(error) => error_response(error),
    }
}

async fn advance(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let (id, job) = path.into_inner();
    let id = match AppInstallationId::parse(id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let job = match AppReference::parse(job) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    match AppEntityRetentionService::new(api.registry.clone())
        .advance_age_cleanup(&authenticated, &id, job, now)
        .await
    {
        Ok(report) => no_store(report),
        Err(error) => error_response(error),
    }
}

fn no_store(value: impl Serialize) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(value)
}

fn error_response(error: AppEntityRetentionStoreError) -> HttpResponse {
    use AppEntityRetentionStoreError::*;
    let (status, code) = match &error {
        CleanupNotFound | MissingInstallation => (StatusCode::NOT_FOUND, "app_cleanup_not_found"),
        ScopeMismatch => (StatusCode::FORBIDDEN, "app_cleanup_scope_mismatch"),
        Contract(_) => (StatusCode::BAD_REQUEST, "invalid_app_cleanup_request"),
        CleanupConfirmationMismatch => (StatusCode::CONFLICT, "app_cleanup_confirmation_mismatch"),
        CleanupPreviewExpired => (StatusCode::GONE, "app_cleanup_preview_expired"),
        CleanupInvalidState => (StatusCode::CONFLICT, "app_cleanup_state_changed"),
        GenerationConflict => (StatusCode::CONFLICT, "app_cleanup_installation_changed"),
        WalCheckpointBusy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_cleanup_checkpoint_busy",
        ),
        Store(AppEntityStoreError::KeysetIndexRequired) => {
            (StatusCode::CONFLICT, "app_cleanup_indexes_not_ready")
        },
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "app_cleanup_failed"),
    };
    api_error(status, code, &error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_cleanup_http_requires_explicit_destructive_confirmation_and_rejects_extra_fields() {
        for value in [
            serde_json::json!({"operation":"confirm"}),
            serde_json::json!({"operation":"confirm","preview_digest":AppDigest::blake3(b"test"),"confirmation":"yes"}),
            serde_json::json!({"operation":"advance"}),
            serde_json::json!({"operation":"pause","scope":"other"}),
        ] {
            let decoded = serde_json::from_value::<ControlRequest>(value);
            assert!(
                decoded.is_err()
                    || decoded
                        .unwrap()
                        .validate_app_contract(&AppContractLimits::default())
                        .is_err()
            );
        }
        let valid: ControlRequest =
            serde_json::from_value(serde_json::json!({"operation":"confirm",
            "preview_digest":AppDigest::blake3(b"test"),"confirmation":"delete_older_app_data"}))
            .unwrap();
        valid
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }
}
