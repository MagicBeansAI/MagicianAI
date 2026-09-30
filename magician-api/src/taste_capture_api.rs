//! Review surface for taste proposals: list what is waiting, approve, reject.
//!
//! The read side mirrors `taste_profile_api`: absence is a normal state, not a
//! fault. The write side is the only path by which a distilled directive ever
//! reaches the injected profile note, so both decisions go through
//! `TasteCaptureService` rather than touching the store directly — the note
//! write and the status write have an ordering that matters, and duplicating
//! it in a handler is how the two copies drift.

use actix_web::{web, HttpRequest, HttpResponse, Result};
use chrono::Utc;
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::taste_capture::{
    global_capture_service, DecisionOutcome, TasteProposal,
};

#[derive(Debug, serde::Deserialize, Default)]
pub struct TasteProposalsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

fn proposal_json(proposal: &TasteProposal) -> serde_json::Value {
    json!({
        "id": proposal.id,
        // "add" or "retract". A retraction asks the owner to confirm removing
        // something already governing their runs, which is a different
        // question from "shall I add this" and must not look identical.
        "kind": match proposal.kind {
            magician::magician_v2::taste_capture::ProposalKind::Add => "add",
            magician::magician_v2::taste_capture::ProposalKind::Retract => "retract",
        },
        "directive": proposal.directive,
        // The evidence is the point of the review. A surface that shows the
        // directive without what it rests on turns approval into
        // rubber-stamping, which the daily cap exists to prevent.
        "evidence": proposal.evidence,
        "destination": proposal.destination,
        "confidence": proposal.confidence,
        "source_session": proposal.source_session,
        "proposed_at": proposal.proposed_at.to_rfc3339(),
    })
}

/// `GET /api/magician/v2/taste-proposals`
pub async fn list_taste_proposals_handler(
    req: HttpRequest,
    query: web::Query<TasteProposalsQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };

    let Some(service) = global_capture_service() else {
        // Capture is off, or this process serves no owner surface. An empty
        // queue is the truthful answer and the same shape the UI renders when
        // there is genuinely nothing waiting.
        return Ok(HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "enabled": false,
            "proposals": [],
        })));
    };

    // One read for both: health rides the list response rather than its own
    // endpoint (a metric on a page nobody opens is a metric nobody reads), and
    // asking for them separately parses the whole store twice per request.
    // Scoped store, not a scoped filter over a shared one: these proposals are
    // distilled from one owner's private transcripts, and a filter is only as
    // good as every query that remembers to apply it.
    match service
        .store_for(&principal, &workspace)
        .await
        .pending_with_stats()
        .await
    {
        Ok((pending, stats)) => {
            let capture_health = service.capture_health(&principal, &workspace).await;
            Ok(HttpResponse::Ok().json(json!({
                "principal": principal,
                "workspace": workspace,
                "enabled": true,
                "capture_health": capture_health.as_ref().ok(),
                "capture_health_unavailable": capture_health.is_err(),
                "proposals": pending.iter().map(proposal_json).collect::<Vec<_>>(),
                "stats": {
                    "pending": stats.pending,
                    "approved": stats.approved,
                    "rejected": stats.rejected,
                    // Null until the first decision — a ratio over zero
                    // samples reads as 0% and would condemn a distiller
                    // nobody has judged yet.
                    "accept_rate": stats.accept_rate,
                    "needs_attention": stats.needs_attention(),
                    "corrupt_lines": stats.corrupt_lines,
                },
            })))
        },
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": format!("could not read the proposal queue: {error}"),
        }))),
    }
}

async fn decide(
    req: HttpRequest,
    query: web::Query<TasteProposalsQuery>,
    id: web::Path<String>,
    approve: bool,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    let Some(service) = global_capture_service() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "taste capture is not enabled in this process",
        })));
    };

    let id = id.into_inner();
    let outcome = if approve {
        service
            .approve(&principal, &workspace, &id, Utc::now())
            .await
    } else {
        service
            .reject(&principal, &workspace, &id, Utc::now())
            .await
    };

    match outcome {
        Ok(DecisionOutcome::Decided { placed }) => Ok(HttpResponse::Ok().json(json!({
            "id": id,
            "status": if approve { "approved" } else { "rejected" },
            // False on approve means the directive was already in the note —
            // a repeat of a decision whose note write had already landed.
            // Reported rather than hidden so a client can tell "done now" from
            // "was already done".
            "placed": placed,
        }))),
        Ok(DecisionOutcome::RetractionRefused(refusal)) => {
            // 409, not 500: nothing failed, the note simply no longer says
            // what the proposal expected. The proposal stays pending on
            // purpose — reporting success would leave the owner believing a
            // directive is gone while it keeps shaping every run.
            let (reason, detail) = match refusal {
                magician::magician_v2::taste_capture::RemovalRefusal::NotPresent => (
                    "not_present",
                    "That line is no longer in your profile note — it was already removed or \
                     reworded. Nothing was changed."
                        .to_string(),
                ),
                magician::magician_v2::taste_capture::RemovalRefusal::Ambiguous(count) => (
                    "ambiguous",
                    format!(
                        "That exact line appears {count} times in your profile note, so which \
                         one to remove is not clear. Nothing was changed — delete the one you \
                         mean by hand."
                    ),
                ),
            };
            Ok(HttpResponse::Conflict().json(json!({
                "id": id,
                "status": "pending",
                "error": detail,
                "reason": reason,
            })))
        },
        Ok(DecisionOutcome::Unknown) => Ok(HttpResponse::NotFound().json(json!({
            "error": format!("no proposal with id `{id}`"),
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": format!("could not record the decision: {error}"),
        }))),
    }
}

/// `POST /api/magician/v2/taste-proposals/{id}/approve`
pub async fn approve_taste_proposal_handler(
    req: HttpRequest,
    query: web::Query<TasteProposalsQuery>,
    id: web::Path<String>,
) -> Result<HttpResponse> {
    decide(req, query, id, true).await
}

/// `POST /api/magician/v2/taste-proposals/{id}/reject`
pub async fn reject_taste_proposal_handler(
    req: HttpRequest,
    query: web::Query<TasteProposalsQuery>,
    id: web::Path<String>,
) -> Result<HttpResponse> {
    decide(req, query, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::test;

    /// With no capture service installed — every test binary, and any
    /// deployment that has not enabled capture — the queue reads as empty
    /// rather than erroring. A red banner is the wrong answer to "you have not
    /// turned this on".
    #[actix_web::test]
    async fn an_uninstalled_service_reports_an_empty_queue() {
        let app = test::init_service(actix_web::App::new().route(
            "/taste-proposals",
            web::get().to(list_taste_proposals_handler),
        ))
        .await;
        let req = test::TestRequest::get()
            .uri("/taste-proposals")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_request();
        let response: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(response["enabled"], serde_json::Value::Bool(false));
        assert_eq!(response["proposals"].as_array().map(|a| a.len()), Some(0));
    }

    /// Scope is required. Proposals are derived from one owner's transcripts;
    /// defaulting the scope would show them to whoever asked without one.
    #[actix_web::test]
    async fn listing_without_scope_is_refused() {
        let app = test::init_service(actix_web::App::new().route(
            "/taste-proposals",
            web::get().to(list_taste_proposals_handler),
        ))
        .await;
        let req = test::TestRequest::get()
            .uri("/taste-proposals")
            .to_request();
        let response = test::call_service(&app, req).await;
        assert!(response.status().is_client_error());
    }

    /// Deciding without a service is 503, not 404: "capture is off" and "no
    /// such proposal" are different problems and a client should not retry the
    /// first as though the id were wrong.
    #[actix_web::test]
    async fn deciding_without_a_service_is_unavailable_not_not_found() {
        let app = test::init_service(actix_web::App::new().route(
            "/taste-proposals/{id}/approve",
            web::post().to(approve_taste_proposal_handler),
        ))
        .await;
        let req = test::TestRequest::post()
            .uri("/taste-proposals/abc/approve")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert_eq!(response.status().as_u16(), 503);
    }
}
