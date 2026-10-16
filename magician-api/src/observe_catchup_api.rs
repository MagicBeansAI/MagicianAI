//! Scoped API for the one automatic Observe catch-up policy and boot ledger.

use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};

use magician::magician_v2::observe_catchup::{
    ObserveCatchUpController, ObserveCatchUpPolicy, PutObserveCatchUpPolicy,
    CATCH_UP_DURATION_OPTIONS, CATCH_UP_LOOKBACK_OPTIONS, CATCH_UP_SOURCE_CAP_OPTIONS,
    CATCH_UP_TOTAL_CAP_OPTIONS,
};
use magician_comms::channel_assist::channel_observe;

use crate::scope::resolve_required_scope;

use crate::observable_sources_api::ScopeQuery;
use crate::observe_connectors_api::ObserveApi;

pub async fn get_observe_catch_up_handler(
    controller: web::Data<Arc<ObserveCatchUpController>>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let status = controller.status(&principal, &workspace).await;
    HttpResponse::Ok().json(serde_json::json!({
        "status": status,
        "options": {
            "lookback_days": CATCH_UP_LOOKBACK_OPTIONS,
            "max_items_per_source": CATCH_UP_SOURCE_CAP_OPTIONS,
            "max_total_items": CATCH_UP_TOTAL_CAP_OPTIONS,
            "max_duration_minutes": CATCH_UP_DURATION_OPTIONS,
        }
    }))
}

pub async fn put_observe_catch_up_handler(
    controller: web::Data<Arc<ObserveCatchUpController>>,
    observe_api: web::Data<ObserveApi>,
    req: HttpRequest,
    body: web::Json<PutObserveCatchUpPolicy>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let expected_revision = body.expected_revision;
    let policy = ObserveCatchUpPolicy::from(body.into_inner());
    let lookback_days = policy.lookback_days;
    let previous_policy = controller.status(&principal, &workspace).await.policy;
    let calendar_bounds_changed = previous_policy.enabled != policy.enabled
        || previous_policy.lookback_days != policy.lookback_days
        || previous_policy.max_items_per_source != policy.max_items_per_source;
    let status = match controller
        .replace_policy(&principal, &workspace, expected_revision, policy)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            let stale = error.to_string().contains("changed (expected revision");
            return HttpResponse::build(if stale {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            })
            .json(serde_json::json!({
                "code": if stale { "revision_conflict" } else { "invalid_request" },
                "error": error.to_string(),
            }));
        },
    };

    // One visible history choice: legacy message-channel readers consume the
    // same value until that field can be removed in a storage migration.
    let mut warnings = Vec::new();
    let mut channel_config =
        channel_observe::load_or_migrate(controller.workspace_layout(), &principal, &workspace)
            .await;
    channel_config.history_lookback_days = lookback_days;
    if let Err(error) = channel_observe::write_channel_observe(
        controller.workspace_layout(),
        &principal,
        &workspace,
        &channel_config,
    )
    .await
    {
        tracing::warn!(principal, workspace, %error, "catch-up policy saved but message history mirror failed");
        warnings.push(
            "The policy was saved, but the legacy message-history mirror could not be refreshed."
                .to_string(),
        );
    }
    if calendar_bounds_changed {
        if let Err(error) = observe_api
            .refresh_calendar_catch_up_window(&principal, &workspace)
            .await
        {
            tracing::warn!(principal, workspace, %error, "catch-up policy saved but calendar schedule refresh failed");
            warnings.push(
                "The policy was saved, but the calendar schedule could not be refreshed yet."
                    .to_string(),
            );
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "status": status,
        "warnings": warnings,
        "options": {
            "lookback_days": CATCH_UP_LOOKBACK_OPTIONS,
            "max_items_per_source": CATCH_UP_SOURCE_CAP_OPTIONS,
            "max_total_items": CATCH_UP_TOTAL_CAP_OPTIONS,
            "max_duration_minutes": CATCH_UP_DURATION_OPTIONS,
        }
    }))
}
