//! **Is anything watching the outcome loop?**
//!
//! Doc: `docs/plans/2026-08-07-opc-outcome-learning.md`. Two workers run in this
//! process — the maturity sweep, which turns an elapsed waiting window into a
//! recorded `silent` observation, and the proposal pass, which turns a filled
//! cohort into a candidate an owner decides on. Both already keep a health
//! snapshot. Until this module existed, neither snapshot was readable from
//! outside the process, so the only way to learn that a sweep had been failing
//! for a week was to read the log.
//!
//! # Absent is not healthy
//!
//! Both handlers refuse with `503` when no health is attached, following
//! `suppression_api::delivery_watch_health_handler`. *"No worker is running"*
//! and *"the worker found nothing"* are opposite facts that render identically
//! as a page of zeroes, and this subsystem's whole history is of the second
//! being reported when the first was true.
//!
//! # What to actually watch
//!
//! - **`acts_unbound` rising** on the maturity route. The sweep found outward
//!   acts and no cohort declaration describes their payload, so it recorded
//!   nothing about them. That is the sweep working and the CONFIG being
//!   incomplete, and it is invisible from the `matured` count alone.
//! - **`acts_without_work` rising.** Acts filed under no work axis at all. The
//!   sweep cannot reach them from either index, whatever the cohorts say.
//! - **`withheld` rising** on the proposal route. Cohorts exist and are below
//!   the sample floors, which is the honest answer and not an error.

use actix_web::{http::StatusCode, web, HttpResponse};

use magician_learning::outcome_learning::{MaturityWorkerHealth, OutcomeProposalHealth};

use crate::web_api::api_error_response;

/// `GET /api/magician/v2/learning/outcomes/maturity/watch`
///
/// The maturity sweep's own account of its last tick.
pub async fn maturity_watch_health_handler(
    health: Option<web::Data<MaturityWorkerHealth>>,
) -> HttpResponse {
    let Some(health) = health else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "maturity_watch_not_wired",
            "no outcome-maturity worker health is attached to this process, so nothing can be \
             said about whether elapsed waiting windows are being observed at all. This is not \
             a healthy state and is deliberately not reported as one.",
            None,
        );
    };
    HttpResponse::Ok().json(health.snapshot().await)
}

/// `GET /api/magician/v2/learning/outcomes/proposals/watch`
///
/// The proposal pass's own account of its last tick, including what it withheld
/// and why. A withheld comparison is the pass working, so the counts are read
/// together with the maturity route's or not at all: a proposal pass finding
/// nothing above a sweep that recorded nothing is one fact, not two.
pub async fn proposal_watch_health_handler(
    health: Option<web::Data<OutcomeProposalHealth>>,
) -> HttpResponse {
    let Some(health) = health else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "outcome_proposal_watch_not_wired",
            "no outcome-proposal worker health is attached to this process, so nothing can be \
             said about whether filled cohorts are reaching an owner at all. This is not a \
             healthy state and is deliberately not reported as one.",
            None,
        );
    };
    HttpResponse::Ok().json(health.snapshot().await)
}

/// Both watches, on the same scope every other v2 route is mounted under.
pub fn configure_outcome_learning_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/learning/outcomes/maturity/watch",
        web::get().to(maturity_watch_health_handler),
    )
    .route(
        "/learning/outcomes/proposals/watch",
        web::get().to(proposal_watch_health_handler),
    );
}
