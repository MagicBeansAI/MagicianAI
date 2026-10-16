//! The runtime's answer to "is this request still open, and where did it
//! come from?" — over the user-request service, the agentic pause store
//! and the chat store the composition root already holds.
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::coordinator::{RequestIdentity, RequestOracle};
use crate::magician_v2::{
    chat::storage::ChatStore, execution::agentic::FullPauseStore, user_requests::UserRequestService,
};

/// Whether this pending pause is the request the delivery record was written
/// for.
///
/// The comparison is against the id the request was **published** under — the
/// ask's own `<storage key>~<ask id>` (`AgenticPauseState::hitl_correlation_id`)
/// — not the pause's storage key. Comparing the key worked only while the two
/// were the same string; once every ask got its own id, no agentic request was
/// ever "still pending", so the coordinator expired every critical alert
/// instead of sending it. A pause written before asks had identities carries an
/// empty correlation and falls back to its key.
///
/// A record written under a bare key while the pause now holds a later ask is
/// deliberately NOT a match: that record's request is the step's earlier
/// question, and alerting for it would point the owner at a prompt that is no
/// longer the one open.
fn pending_answers_request(
    pending: &crate::magician_v2::execution::agentic::PendingPauseInfo,
    request: &RequestIdentity,
) -> bool {
    let pending_correlation = if pending.hitl_correlation_id.is_empty() {
        pending.key.as_str()
    } else {
        pending.hitl_correlation_id.as_str()
    };
    pending_correlation == request.correlation_id
        && pending
            .principal
            .as_deref()
            .map_or(true, |principal| principal == request.principal)
        && pending
            .workspace
            .as_deref()
            .map_or(true, |workspace| workspace == request.workspace)
}

pub struct RuntimeRequestOracle {
    user_requests: Arc<UserRequestService>,
    pause_store: Option<Arc<FullPauseStore>>,
    chat_store: Option<Arc<dyn ChatStore>>,
}

impl RuntimeRequestOracle {
    pub fn new(
        user_requests: Arc<UserRequestService>,
        pause_store: Option<Arc<FullPauseStore>>,
        chat_store: Option<Arc<dyn ChatStore>>,
    ) -> Self {
        Self {
            user_requests,
            pause_store,
            chat_store,
        }
    }
}

#[async_trait]
impl RequestOracle for RuntimeRequestOracle {
    async fn still_pending(&self, request: &RequestIdentity) -> bool {
        match request.source.as_str() {
            "agentic" => {
                // An agentic pause is keyed by its pause-state id; the
                // execution's pending set is the authority. Without an
                // execution id nothing can be checked, so the request is
                // treated as closed rather than alerted blind.
                let (Some(store), Some(execution_id)) =
                    (&self.pause_store, request.execution_id.as_deref())
                else {
                    return false;
                };
                store
                    .get_pending_for_execution(execution_id)
                    .iter()
                    .any(|pending| pending_answers_request(pending, request))
            },
            _ => self
                .user_requests
                .pending_request_snapshot(
                    &request.correlation_id,
                    Some(&request.principal),
                    Some(&request.workspace),
                )
                .await
                .is_some(),
        }
    }

    async fn origin_channel(
        &self,
        request: &RequestIdentity,
        input_schema: Option<&Value>,
    ) -> Option<(String, String)> {
        let chat_store = self.chat_store.as_ref()?;
        let context = input_schema?.get("context")?;
        let session_id = context
            .get("chat_session_id")
            .or_else(|| context.get("session_id"))
            .and_then(Value::as_str)?;
        let session = chat_store.get_session(session_id).await.ok().flatten()?;
        if session.principal != request.principal || session.workspace != request.workspace {
            return None;
        }
        let address = session.origin_channel.address.clone()?;
        Some((session.origin_channel.channel_type.clone(), address))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::{PendingPauseInfo, UserInputType};

    fn request(correlation_id: &str) -> RequestIdentity {
        RequestIdentity {
            principal: "owner".to_string(),
            workspace: "ws".to_string(),
            correlation_id: correlation_id.to_string(),
            source: "agentic".to_string(),
            execution_id: Some("exec-1".to_string()),
        }
    }

    fn pending(correlation_id: &str) -> PendingPauseInfo {
        let mut pending = PendingPauseInfo::for_test(
            "exec-1:plan-1:step-1",
            "exec-1",
            UserInputType::Password { placeholder: None },
        );
        pending.hitl_correlation_id = correlation_id.to_string();
        pending.principal = Some("owner".to_string());
        pending.workspace = Some("ws".to_string());
        pending
    }

    /// The alert for a credential ask is only sent while that ask is open, and
    /// "that ask" is the id it was published under. Comparing the pause's
    /// storage key instead expired every critical agentic alert once each ask
    /// got its own correlation id.
    #[test]
    fn a_pending_ask_answers_the_request_published_under_its_own_id() {
        let ask = "exec-1:plan-1:step-1~0123456789ab";
        assert!(pending_answers_request(&pending(ask), &request(ask)));

        // The step's earlier question is not this one.
        assert!(!pending_answers_request(
            &pending(ask),
            &request("exec-1:plan-1:step-1")
        ));
        assert!(!pending_answers_request(
            &pending(ask),
            &request("exec-1:plan-1:step-1~ffffffffffff")
        ));

        // A pause written before asks had identities carries no correlation and
        // is addressed by its key.
        let mut legacy = pending("");
        legacy.hitl_correlation_id = String::new();
        assert!(pending_answers_request(
            &legacy,
            &request("exec-1:plan-1:step-1")
        ));

        // Another scope's row never answers this request.
        let mut foreign = pending(ask);
        foreign.principal = Some("someone-else".to_string());
        assert!(!pending_answers_request(&foreign, &request(ask)));
    }
}
