//! Which proposed acts get an action-level alignment check in v1.
//!
//! Owner decision 2026-10-09 (P0 contracts §8.1): v1 checks **outward and
//! commitment acts** — sends, invites, submissions, disclosures, orders,
//! bookings, purchases. Everything the existing classifiers call
//! `PrivateLocal` gets request-level steering only.
//!
//! This is structural classification by the same two classifiers the outward
//! gate and the approval envelopes already use, read argument-aware, so a send
//! hidden behind a passthrough (`gmail raw +send`) counts. It is not a
//! preference rule: nothing here looks at what the owner wants.
//!
//! # Published limit
//!
//! `PrivateLocal` is the classifiers' default fall-through, so an act whose
//! effect nobody has classified is not checked — including a purchase made
//! through raw HTTP or shell. Coverage widens by classifying capabilities in
//! `agents::outward_actions` / `agents::consequence_class`, never by adding a
//! keyword here.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::agents::consequence_class::{
    consequence_class_for_dispatch, ConsequenceClass,
};
use crate::magician_v2::agents::outward_actions::{dispatch_action_token, outward_dispatch_class};
use crate::magician_v2::execution::actions::ExecutableAction;

/// How alignment treats one proposed act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AlignmentScope {
    /// An action-level check is required before admission.
    Checked {
        capability: String,
        action: String,
        consequence_class: ConsequenceClass,
        /// `OutwardClass::as_str` token, when the act is outward.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outward_class: Option<String>,
    },
    /// Request-level steering only; no action-level hold in v1.
    SteeredOnly,
}

impl AlignmentScope {
    pub fn is_checked(&self) -> bool {
        matches!(self, Self::Checked { .. })
    }
}

/// Classify a pack dispatch by its capability, action token and arguments.
pub fn classify_dispatch(
    capability: &str,
    resolved_params: &HashMap<String, Value>,
) -> AlignmentScope {
    let action = dispatch_action_token(capability, resolved_params);
    let outward_class = outward_dispatch_class(capability, &action, resolved_params);
    let consequence_class = consequence_class_for_dispatch(capability, &action, resolved_params);
    if outward_class.is_none() && consequence_class == ConsequenceClass::PrivateLocal {
        return AlignmentScope::SteeredOnly;
    }
    AlignmentScope::Checked {
        capability: capability.to_string(),
        action,
        consequence_class,
        outward_class: outward_class.map(|class| class.as_str().to_string()),
    }
}

/// Classify an executable action. Only `Pack` actions carry a capability the
/// classifiers know; native file/HTTP/bash/DuckDB actions are opaque and, by
/// the v1 decision, steered only.
pub fn classify_action(action: &ExecutableAction) -> AlignmentScope {
    match action {
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => classify_dispatch(capability_name, resolved_params),
        _ => AlignmentScope::SteeredOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(value: Value) -> HashMap<String, Value> {
        value
            .as_object()
            .cloned()
            .map(|map| map.into_iter().collect())
            .unwrap_or_default()
    }

    #[test]
    fn sends_and_commitments_are_checked() {
        assert!(
            classify_dispatch("imessage_send", &params(json!({"to": "x", "text": "hi"})))
                .is_checked()
        );
        assert!(classify_dispatch(
            "gmail",
            &params(json!({"action": "raw", "args": ["+send", "--to", "a@example.test"]}))
        )
        .is_checked());
    }

    #[test]
    fn local_work_is_steered_only() {
        assert_eq!(
            classify_dispatch("search_memory", &params(json!({"query": "diet"}))),
            AlignmentScope::SteeredOnly
        );
        assert_eq!(
            classify_dispatch(
                "gmail",
                &params(json!({"action": "search", "query": "invoice"}))
            ),
            AlignmentScope::SteeredOnly
        );
    }

    #[test]
    fn opaque_native_actions_are_steered_only_in_v1() {
        let http = ExecutableAction::Http(crate::magician_v2::execution::actions::HttpAction::get(
            "https://example.invalid/checkout",
        ));
        assert_eq!(classify_action(&http), AlignmentScope::SteeredOnly);
    }
}
