use super::UserRequest;

impl UserRequest {
    /// Enrich the read model without changing durable request bytes, proposal
    /// hashes or idempotent replay. Opening a review never answers the request.
    pub fn for_presentation(mut self) -> Self {
        if self.request_type == crate::magician_v2::relentless_runtime::TEXT_PROPOSAL_REQUEST_TYPE {
            if let Some(context) = self.context.as_object_mut() {
                let id: String = url::form_urlencoded::byte_serialize(self.id.as_bytes()).collect();
                context.insert(
                    "review_href".into(),
                    format!("/work?view=quality&proposal={id}").into(),
                );
                context.insert("review_label".into(), "Review and start".into());
                self.question = "A Relentless run is proposed. Review its requirements, models and budget before starting. No review run has started. Dismiss declines this proposal.".into();
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proposal_review_is_navigation_and_preserves_stored_request_and_decisions() {
        let request: UserRequest = serde_json::from_value(json!({
            "id":"proposal&other=1", "request_type":"relentless.contract_proposal",
            "question":"old notice", "options":[{"id":"dismiss","label":"Dismiss notice","requires_input":false}],
            "principal":"owner", "workspace":"default", "context":{"proposal_hash":"frozen"},
            "source":"relentless", "timeout_secs":86400, "default_on_timeout":"dismiss", "created_at":1
        })).unwrap();
        let presented = request.clone().for_presentation();
        assert_eq!(
            presented.context["review_href"],
            "/work?view=quality&proposal=proposal%26other%3D1"
        );
        assert_eq!(presented.context["review_label"], "Review and start");
        assert_eq!(presented.options, request.options);
        assert_eq!(
            presented.context["proposal_hash"],
            request.context["proposal_hash"]
        );
        assert!(request.context.get("review_href").is_none());
        assert_eq!(request.question, "old notice");
    }
}
