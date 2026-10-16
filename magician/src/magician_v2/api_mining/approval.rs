//! Shared HITL contract for write replay on both API-mining rails.

use super::capability::SideEffects;
use super::recipe::request_shape_fingerprint;
use super::replay_grants::{is_denylisted_url_template, GrantKey, ReplayGrantStore};
use crate::magician_v2::user_requests::service::{RequestOption, UserRequest};

pub const REQUEST_TYPE: &str = "api_replay_approval";
pub const OPTION_APPROVE_ALWAYS_STEP: &str = "approve_always_step";
pub const OPTION_APPROVE_ONCE: &str = "approve_once";
pub const OPTION_DENY: &str = "deny";
pub const TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct ApprovalSubject {
    pub grant_key: GrantKey,
    pub origin: String,
    pub method: String,
    pub url_template: String,
    pub side_effects: SideEffects,
    pub request_preview: serde_json::Value,
    /// False for an opaque or structurally unbounded body. Such a request can
    /// be approved once but cannot safely share a value-free durable grant.
    pub durable_grant_allowed: bool,
    pub policy_reason: String,
    pub owner_agent_id: Option<String>,
}

pub fn options_for(url_template: &str) -> Vec<RequestOption> {
    let mut options = Vec::with_capacity(3);
    if !is_denylisted_url_template(url_template) {
        options.push(RequestOption {
            id: OPTION_APPROVE_ALWAYS_STEP.into(),
            label: "Approve and remember for this step".into(),
            requires_input: false,
        });
    }
    options.push(RequestOption {
        id: OPTION_APPROVE_ONCE.into(),
        label: "Approve once".into(),
        requires_input: false,
    });
    options.push(RequestOption {
        id: OPTION_DENY.into(),
        label: "Deny".into(),
        requires_input: false,
    });
    options
}

pub fn build_api_replay_approval_request(
    subject: &ApprovalSubject,
    principal: &str,
    workspace: &str,
    execution_id: Option<String>,
    task_id: Option<String>,
) -> UserRequest {
    UserRequest {
        id: String::new(),
        request_type: REQUEST_TYPE.into(),
        question: format!(
            "Replay this {} request to {} directly over the API instead of the browser?",
            subject.method, subject.origin
        ),
        options: if subject.durable_grant_allowed {
            options_for(&subject.url_template)
        } else {
            vec![
                RequestOption {
                    id: OPTION_APPROVE_ONCE.into(),
                    label: "Approve once".into(),
                    requires_input: false,
                },
                RequestOption {
                    id: OPTION_DENY.into(),
                    label: "Deny".into(),
                    requires_input: false,
                },
            ]
        },
        principal: principal.into(),
        workspace: workspace.into(),
        context: serde_json::json!({
            "kind": "api_replay_write",
            "origin": subject.origin,
            "method": subject.method,
            "url_template": approval_url_shape(&subject.url_template),
            "side_effects": subject.side_effects,
            "request_preview": subject.request_preview,
            "grant_key": subject.grant_key,
            "policy_reason": subject.policy_reason,
            "owner_agent_id": subject.owner_agent_id,
            "always_ask": !subject.durable_grant_allowed
                || is_denylisted_url_template(&subject.url_template),
            "durable_grant_allowed": subject.durable_grant_allowed
                && !is_denylisted_url_template(&subject.url_template),
        }),
        source: "api_mining".into(),
        execution_id,
        task_id,
        timeout_secs: TIMEOUT_SECS,
        default_on_timeout: OPTION_DENY.into(),
        created_at: 0,
        sensitive: None,
    }
}

pub fn decision_is_approval(decision: &str) -> bool {
    matches!(decision, OPTION_APPROVE_ONCE | OPTION_APPROVE_ALWAYS_STEP)
}

/// Durable approvals must stay revocable until each send. Do not also mint
/// the independent one-run capability when the user chooses to remember one.
pub fn decision_is_one_run_approval(decision: &str) -> bool {
    decision == OPTION_APPROVE_ONCE
}

/// Render an approval-safe URL shape. Concrete paths, query values, and
/// fragments may contain account identifiers or task data, so the HITL record
/// retains only the origin, path depth, and query-key set.
pub fn approval_url_shape(value: &str) -> String {
    let Ok(url) = url::Url::parse(value) else {
        return "<invalid-url>".into();
    };
    let path_segments = url.path_segments().into_iter().flatten().count();
    let mut query_keys = Vec::new();
    let mut query_supported = true;
    for (index, (key, _)) in url.query_pairs().enumerate() {
        if index >= 64 || key.is_empty() || key.len() > 128 {
            query_supported = false;
            break;
        }
        query_keys.push(key.into_owned());
    }
    query_keys.sort_unstable();
    query_keys.dedup();
    let mut shape = format!(
        "{}/<{} path segment{}>",
        url.origin().ascii_serialization(),
        path_segments,
        if path_segments == 1 { "" } else { "s" }
    );
    if !query_supported {
        shape.push_str("?<query shape unsupported>");
    } else if !query_keys.is_empty() {
        shape.push('?');
        shape.push_str(
            &query_keys
                .into_iter()
                .map(|key| format!("{key}=[REDACTED]"))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    shape
}

pub fn apply_decision(
    grants: &ReplayGrantStore,
    subject: &ApprovalSubject,
    decision: &str,
    request_id: &str,
) -> Result<bool, String> {
    if decision == OPTION_APPROVE_ALWAYS_STEP {
        if !subject.durable_grant_allowed {
            return Err(
                "opaque or structurally unbounded request bodies cannot receive a durable grant"
                    .into(),
            );
        }
        grants.grant_for_url(&subject.grant_key, &subject.url_template, Some(request_id))?;
    }
    Ok(decision_is_approval(decision))
}

/// Fingerprint a concrete replay request without retaining its values.
pub fn replay_request_shape_fingerprint(
    method: &str,
    concrete_url: &str,
    body: Option<&str>,
) -> String {
    let template = url::Url::parse(concrete_url)
        .map(|mut url| {
            let segments: Vec<_> = url
                .path_segments()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, segment)| {
                    if segment.chars().any(|character| character.is_ascii_digit())
                        || segment.len() >= 16
                    {
                        format!("{{p{index}}}")
                    } else {
                        segment.to_owned()
                    }
                })
                .collect();
            url.set_path(&segments.join("/"));
            let query: Vec<_> = url
                .query_pairs()
                .map(|(name, _)| format!("{name}={{{name}}}"))
                .collect();
            url.set_query((!query.is_empty()).then(|| query.join("&")).as_deref());
            url.to_string().replace("%7B", "{").replace("%7D", "}")
        })
        .unwrap_or_else(|_| concrete_url.to_owned());
    request_shape_fingerprint(method, &template, body)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn remembered_approval_does_not_mint_an_irrevocable_one_run_capability() {
        assert!(decision_is_one_run_approval(OPTION_APPROVE_ONCE));
        assert!(!decision_is_one_run_approval(OPTION_APPROVE_ALWAYS_STEP));
        assert!(!decision_is_one_run_approval(OPTION_DENY));
    }

    #[test]
    fn approval_url_shape_contains_no_path_query_or_fragment_values() {
        let shape = approval_url_shape(
            "https://api.example.test/users/private-user/orders/42?token=secret&page=1#private",
        );

        assert!(shape.starts_with("https://api.example.test/<4 path segments>"));
        assert!(shape.contains("page=[REDACTED]"));
        assert!(shape.contains("token=[REDACTED]"));
        for private in [
            "private-user",
            "orders",
            "42",
            "secret",
            "page=1",
            "#private",
        ] {
            assert!(!shape.contains(private));
        }
    }

    #[test]
    fn opaque_body_approval_has_no_durable_option() {
        let subject = ApprovalSubject {
            grant_key: GrantKey {
                recipe_id: Some("recipe".into()),
                step_id: Some("step".into()),
                capability_id: None,
                request_shape_fingerprint: "shape".into(),
            },
            origin: "https://api.example.test".into(),
            method: "POST".into(),
            url_template: "https://api.example.test/items".into(),
            side_effects: SideEffects::Write,
            request_preview: serde_json::json!({"body_shape": ["opaque"]}),
            durable_grant_allowed: false,
            policy_reason: "write_replay_requires_grant".into(),
            owner_agent_id: None,
        };

        let request = build_api_replay_approval_request(&subject, "owner", "default", None, None);
        assert_eq!(
            request
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec![OPTION_APPROVE_ONCE, OPTION_DENY]
        );
        assert_eq!(request.context["always_ask"], true);
        assert_eq!(request.context["durable_grant_allowed"], false);
    }
}
