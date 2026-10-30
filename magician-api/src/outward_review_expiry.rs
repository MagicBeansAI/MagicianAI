//! Settles reviews-before-sending nobody answered (`agents::outward_review`).
//!
//! A review left past its deadline (7 days) is answered "discard" through
//! the very resume path an owner's answer runs, so the run continues told
//! nothing was sent. A late Send is refused at resume as well; this closes
//! the reviews nobody answers.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use actix_web::web;
use tracing::{info, warn};

use magician::magician_v2::agents::outward_review::OPTION_DISCARD;

use crate::web_api::{AgenticResumeRequest, AgenticResumeValue, MagicianV2Api};

const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// The answer channel an expiry is stamped with.
const EXPIRY_CHANNEL: &str = "outward_review_expiry";

static INSTALLED: OnceLock<()> = OnceLock::new();

/// Start the sweep on the first API instance; later calls do nothing.
pub fn install(api: Arc<MagicianV2Api>) {
    if INSTALLED.set(()).is_err() {
        return;
    }
    actix_web::rt::spawn(async move {
        let mut ticker = actix_web::rt::time::interval(SWEEP_INTERVAL);
        loop {
            ticker.tick().await;
            let settled = sweep(&api, chrono::Utc::now().timestamp_millis()).await;
            if settled > 0 {
                info!(settled, "[OUTWARD-REVIEW] discarded unanswered reviews");
            }
        }
    });
}

async fn sweep(api: &MagicianV2Api, now_ms: i64) -> usize {
    let mut settled = 0;
    for (execution_id, correlation_id, principal, workspace) in
        api.full_pause_store().expired_outward_reviews(now_ms)
    {
        let request = AgenticResumeRequest {
            pause_state_id: Some(correlation_id),
            plan_id: None,
            step_id: None,
            input_type: "confirmation".to_string(),
            value: AgenticResumeValue::Choice {
                selected_id: OPTION_DISCARD.to_string(),
                other_value: None,
            },
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            answer_channel: Some(EXPIRY_CHANNEL.to_string()),
        };
        let scope = principal.zip(workspace);
        // Acknowledged once admitted: the continuation runs on, so one long
        // run never holds up the other expired reviews.
        match api
            .resume_agentic_execution_acknowledged_when_admitted(
                web::Path::from(execution_id.clone()),
                web::Json(request),
                scope,
                "agentic".to_string(),
            )
            .await
        {
            Ok(response) if response.status().is_success() => settled += 1,
            Ok(response) => warn!(
                execution_id = %execution_id,
                status = %response.status(),
                "[OUTWARD-REVIEW] could not settle an expired review"
            ),
            Err(error) => warn!(
                execution_id = %execution_id,
                error = %error,
                "[OUTWARD-REVIEW] could not settle an expired review"
            ),
        }
    }
    settled
}
