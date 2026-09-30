//! Read-only mirror of the owner's taste profile.
//!
//! The note is the single writable copy — Settings and `/memory` render what
//! is injected and point the owner at their notes app to change it. There is
//! deliberately no write path here: a second editor is how two copies of the
//! same preference start disagreeing, and the design settled that question
//! (the note is canonical; Magician-mediated writes are Slice 2's approval
//! queue, not a REST PUT).

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::taste_profile::global_taste_profile_loader;

#[derive(Debug, serde::Deserialize, Default)]
pub struct TasteProfileScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `GET /api/magician/v2/taste-profile`
///
/// Returns exactly what prompt assembly would inject right now, plus the
/// ceiling state the mirrors warn on. `injectable` is `null` when no profile
/// exists — the same non-answer prompt assembly gets — rather than an error,
/// because "you have not written one yet" is a normal state, not a fault.
pub async fn get_taste_profile_handler(
    req: HttpRequest,
    query: web::Query<TasteProfileScopeQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };

    let Some(loader) = global_taste_profile_loader() else {
        // No loader installed in this process. Report the feature as absent
        // rather than 500ing: a mirror that cannot read is not an error the
        // owner can act on, and it is the same shape as "no note yet".
        return Ok(HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "enabled": false,
            "injectable": serde_json::Value::Null,
            "version": serde_json::Value::Null,
            "over_ceiling": false,
            "note_path": serde_json::Value::Null,
            "max_chars": serde_json::Value::Null,
        })));
    };

    let settings = loader.settings().clone();
    let snapshot = loader.load(&principal, &workspace).await;
    Ok(HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "enabled": settings.enabled,
        "injectable": snapshot.as_ref().map(|s| s.injectable.clone()),
        "version": snapshot.as_ref().map(|s| s.version.clone()),
        "over_ceiling": snapshot.as_ref().is_some_and(|s| s.over_ceiling),
        "chars": snapshot.as_ref().map(|s| s.injectable.chars().count()),
        "note_path": settings.note_path,
        "max_chars": settings.max_chars,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::test;

    /// In a process with no loader installed — every test binary, and any
    /// tool that never serves prompts — the mirror answers "absent", not an
    /// error. A 500 here would put a red banner on the Settings page of a
    /// deployment that simply has not written a profile.
    #[actix_web::test]
    async fn an_uninstalled_loader_reports_absence_rather_than_failing() {
        let app = test::init_service(
            actix_web::App::new().route("/taste-profile", web::get().to(get_taste_profile_handler)),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/taste-profile")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_request();
        let response: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(response["enabled"], serde_json::Value::Bool(false));
        assert_eq!(response["injectable"], serde_json::Value::Null);
        assert_eq!(response["over_ceiling"], serde_json::Value::Bool(false));
    }

    /// Scope is required: the profile is per-owner, and a mirror that
    /// silently defaulted the scope would show one owner's directives to
    /// whoever asked without one.
    #[actix_web::test]
    async fn a_request_without_scope_is_refused() {
        let app = test::init_service(
            actix_web::App::new().route("/taste-profile", web::get().to(get_taste_profile_handler)),
        )
        .await;
        let req = test::TestRequest::get().uri("/taste-profile").to_request();
        let response = test::call_service(&app, req).await;
        assert!(
            response.status().is_client_error(),
            "missing scope must not resolve to a default owner"
        );
    }
}
