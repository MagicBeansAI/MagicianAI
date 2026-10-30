//! `request_preference_wording` — the internal analyst asks the owner to see
//! the wording of their preferences for this one run.
//!
//! The analyst reads preferences redacted. This raises a non-blocking user
//! request of type `alignment.wording_unlock`; the owner answers it on a
//! private surface (web, iOS — the HITL respond route refuses any other), and
//! an approval grants wording to this run's execution id only
//! (`preference_alignment::wording_grant`).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::preference_alignment::wording_grant::{
    WORDING_UNLOCK_APPROVE, WORDING_UNLOCK_DECLINE, WORDING_UNLOCK_REQUEST_TYPE,
};
use crate::magician_v2::user_requests::{RequestOption, UserRequest};

use super::shared::{require_scope_str, scope_arg_str};

/// How long the owner has to answer; unanswered means declined.
const UNLOCK_TIMEOUT_SECS: u64 = 900;

/// The user request the analyst's ask becomes, or why it cannot be asked.
pub fn unlock_request(args: &Value) -> Result<UserRequest, String> {
    let principal = require_scope_str(args, "__principal", "request_preference_wording")
        .map_err(|error| error.to_string())?;
    let workspace = require_scope_str(args, "__workspace", "request_preference_wording")
        .map_err(|error| error.to_string())?;
    let execution_id = scope_arg_str(args, "__execution_id")
        .filter(|id| !id.is_empty())
        .ok_or("request_preference_wording needs the run's execution id; a grant is for one run")?;
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .ok_or("request_preference_wording requires a non-empty `reason`")?;
    Ok(UserRequest {
        id: String::new(),
        request_type: WORDING_UNLOCK_REQUEST_TYPE.to_string(),
        question: format!(
            "Your analyst asks to read the wording of your preferences for this one \
             investigation.\n\nWhy: {reason}"
        ),
        options: vec![
            RequestOption {
                id: WORDING_UNLOCK_APPROVE.to_string(),
                label: "Allow for this investigation".to_string(),
                requires_input: false,
            },
            RequestOption {
                id: WORDING_UNLOCK_DECLINE.to_string(),
                label: "Keep it hidden".to_string(),
                requires_input: false,
            },
        ],
        principal,
        workspace,
        context: json!({
            "reason": reason,
            "owner_agent_id": scope_arg_str(args, "__agent_id").unwrap_or_default(),
        }),
        source: "analyst".to_string(),
        execution_id: Some(execution_id),
        task_id: scope_arg_str(args, "__task_id"),
        timeout_secs: UNLOCK_TIMEOUT_SECS,
        default_on_timeout: WORDING_UNLOCK_DECLINE.to_string(),
        created_at: 0,
        sensitive: None,
    })
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(user_request_service) = resources.user_request_service.clone() else {
        return Ok(json!({
            "status": "error",
            "reason": "request_preference_wording: user request service is not configured",
        }));
    };
    let request = match unlock_request(&args) {
        Ok(request) => request,
        Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
    };
    match user_request_service.submit_nonblocking(request).await {
        Ok(submission) => Ok(json!({
            "status": "pending",
            "request_id": submission.request_id(),
            "note": "Wording stays redacted until the owner allows it on their web or iOS app; \
                     re-read with internal_data after they answer.",
        })),
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("request_preference_wording: submission failed: {error}"),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Value {
        json!({
            "__principal": "owner", "__workspace": "default",
            "__execution_id": "exec-analyst", "__agent_id": "internal-system-analyst",
            "reason": "A dinner order was held twice; I need to see which preference matched."
        })
    }

    #[test]
    fn the_ask_is_a_wording_unlock_for_this_run_declined_by_default() {
        let request = unlock_request(&args()).unwrap();
        assert_eq!(request.request_type, WORDING_UNLOCK_REQUEST_TYPE);
        assert_eq!(request.execution_id.as_deref(), Some("exec-analyst"));
        assert_eq!(request.default_on_timeout, WORDING_UNLOCK_DECLINE);
        assert!(request
            .options
            .iter()
            .any(|option| option.id == WORDING_UNLOCK_APPROVE));
        assert!(request.question.contains("held twice"));
    }

    #[test]
    fn no_reason_or_no_run_means_no_ask() {
        let mut no_reason = args();
        no_reason["reason"] = json!("  ");
        assert!(unlock_request(&no_reason).is_err());
        let mut no_run = args();
        no_run.as_object_mut().unwrap().remove("__execution_id");
        assert!(unlock_request(&no_run).is_err());
    }
}
