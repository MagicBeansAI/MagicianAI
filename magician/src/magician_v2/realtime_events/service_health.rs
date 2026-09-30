//! Scoped, content-free health notices on the existing durable HITL lifecycle.
//! Raw provider errors may contain secrets or request bodies and never enter a notice.
use super::{RuntimeTransportBroadcaster, RuntimeTransportEvent};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceFailure {
    Authentication,
    Credit,
    RateLimit,
    Unavailable,
}

impl ServiceFailure {
    pub fn from_error(error: &str) -> Option<Self> {
        let text = error.to_ascii_lowercase();
        if [
            "insufficient_quota",
            "insufficient quota",
            "insufficient credits",
            "credit balance",
            "out of credits",
            "billing",
            "payment required",
            "status 402",
            "http 402",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Some(Self::Credit)
        } else if [
            "no api key",
            "api key not",
            "api_key is missing",
            "invalid api key",
            "invalid_api_key",
            "unauthorized",
            "authentication",
            "not logged in",
            "not signed in",
            "not authenticated",
            "login required",
            "status 401",
            "http 401",
            "status: 401",
            "status 403",
            "http 403",
            "status: 403",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Some(Self::Authentication)
        } else if [
            "rate limit",
            "rate_limit",
            "too many requests",
            "individual quota reached",
            "status 429",
            "http 429",
            "status: 429",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Some(Self::RateLimit)
        } else if [
            "connection refused",
            "connection reset",
            "connect error",
            "error sending request",
            "timed out",
            "timeout",
            "service unavailable",
            "provider unavailable",
            "bad gateway",
            "status 500",
            "http 500",
            "status 502",
            "http 502",
            "status 503",
            "http 503",
            "status 504",
            "http 504",
            "status: 500",
            "status: 502",
            "status: 503",
            "status: 504",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Some(Self::Unavailable)
        } else {
            None
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Credit => "credit",
            Self::RateLimit => "rate_limit",
            Self::Unavailable => "unavailable",
        }
    }
    fn guidance(self, service: &str) -> String {
        match self {
            Self::Authentication if service.starts_with("harness:pi") => "Pi needs credentials for its selected model. Without a Magician profile, Pi uses its own default model and login. Sign in through Pi, or select a configured Magician profile for this operation, then retry.".into(),
            Self::Authentication => format!("{service} could not authenticate. Check its sign-in or configured credentials, then retry."),
            Self::Credit => format!("{service} reported a billing or credit limit. Check the provider account's balance and limits, then retry."),
            Self::RateLimit => format!("{service} is rate limited. Requests may be delayed or use a configured fallback. Wait for the limit to reset before retrying."),
            Self::Unavailable => format!("{service} is currently unreachable or timed out. Check the service or connection. Configured fallback may keep some work running."),
        }
    }
}

#[derive(Clone)]
struct Incident {
    id: String,
    failure: ServiceFailure,
    last_seen: i64,
}
#[derive(Default)]
pub(super) struct HealthNotices {
    active: HashMap<(String, String, String), Incident>,
    clock: i64,
}

fn service_label(value: &str) -> Option<String> {
    let value = value.trim().replace("harness-", "harness:");
    (!value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || "_:- ./[]".contains(c)))
    .then_some(value)
}

impl RuntimeTransportBroadcaster {
    /// One notice per scoped service incident, including failures in background work.
    /// Dismissal acknowledges the notice; it never retries work or changes credentials.
    pub fn report_service_health(
        &self,
        principal: &str,
        workspace: &str,
        service: &str,
        result: Result<(), ServiceFailure>,
    ) {
        if principal.is_empty() || workspace.is_empty() {
            return;
        }
        let Some(service) = service_label(service) else {
            return;
        };
        let key = (principal.to_owned(), workspace.to_owned(), service.clone());
        let mut notices = self
            .service_health_notices
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut now = chrono::Utc::now()
            .timestamp_millis()
            .max(notices.clock.saturating_add(1));
        notices.clock = now;
        // Stable scoped key: recover an outstanding notice by exact lookup, never
        // replay the journal or load provider history at startup.
        let id = format!("service_health:{:x}", Sha256::digest(service.as_bytes()));
        if !notices.active.contains_key(&key) && notices.active.len() >= 1024 {
            if let Some(oldest) = notices
                .active
                .iter()
                .min_by_key(|(_, incident)| incident.last_seen)
                .map(|(key, _)| key.clone())
            {
                notices.active.remove(&oldest);
            }
        }
        if !notices.active.contains_key(&key) {
            if let Some(RuntimeTransportEvent::HitlRequested {
                input_schema: Some(schema),
                timestamp,
                ..
            }) = self.pending_hitl_request(principal, workspace, &id)
            {
                let failure = match schema["health_issue"].as_str() {
                    Some("authentication") => Some(ServiceFailure::Authentication),
                    Some("credit") => Some(ServiceFailure::Credit),
                    Some("rate_limit") => Some(ServiceFailure::RateLimit),
                    Some("unavailable") => Some(ServiceFailure::Unavailable),
                    _ => None,
                };
                if let Some(failure) = failure {
                    now = now.max(timestamp.saturating_add(1));
                    notices.clock = now;
                    notices.active.insert(
                        key.clone(),
                        Incident {
                            id: id.clone(),
                            failure,
                            last_seen: timestamp,
                        },
                    );
                }
            }
        }
        if let Some(existing) = notices.active.get_mut(&key) {
            if result == Err(existing.failure) {
                existing.last_seen = now;
                return;
            }
            let event = RuntimeTransportEvent::HitlResolved {
                correlation_id: existing.id.clone(),
                source: "service_health".into(),
                outcome: if result.is_ok() {
                    "responded"
                } else {
                    "cancelled"
                }
                .into(),
                decision: Some(
                    if result.is_ok() {
                        "service_recovered"
                    } else {
                        "health_issue_changed"
                    }
                    .into(),
                ),
                task_id: None,
                execution_id: None,
                agent_id: None,
                principal: Some(principal.into()),
                workspace: Some(workspace.into()),
                timestamp: now,
            };
            let already_resolved = matches!(
                self.hitl_lifecycle_state(principal, workspace, &existing.id),
                super::HitlLifecycleState::Resolved
            );
            if !already_resolved && !self.emit_hitl_lifecycle_if_accepted(event) {
                return;
            }
            notices.active.remove(&key);
        }
        let Err(failure) = result else {
            return;
        };
        let event=RuntimeTransportEvent::HitlRequested {
            correlation_id:id.clone(),source:"service_health".into(),input_type:"choice".into(),
            prompt:failure.guidance(&service),hint:Some("This notice does not change engine settings or retry completed work. It clears after a successful call, or you can dismiss it.".into()),
            input_schema:Some(json!({"type":"choice","options":[{"id":"dismiss","label":"Dismiss"}],"allow_other":false,"service":service,"health_issue":failure.code()})),
            task_id:None,execution_id:None,agent_id:None,
            principal:Some(principal.into()),workspace:Some(workspace.into()),timestamp:now.saturating_add(1),
        };
        notices.clock = now.saturating_add(1);
        if self.emit_hitl_lifecycle_if_accepted(event) {
            notices.active.insert(
                key,
                Incident {
                    id,
                    failure,
                    last_seen: now,
                },
            );
        }
    }

    pub(super) fn observe_provider_health(&self, event: &RuntimeTransportEvent) {
        if let RuntimeTransportEvent::LLMResponseReceived {
            principal: Some(principal),
            workspace: Some(workspace),
            provider,
            correlation,
            profile,
            success,
            error,
            ..
        } = event
        {
            if provider.is_empty()
                || correlation
                    .as_ref()
                    .is_some_and(|call| call.response_reused)
            {
                return;
            }
            // A working profile must not clear another profile's missing credentials.
            let service = match profile.as_deref().filter(|name| !name.is_empty()) {
                Some(profile) => format!("{provider} [{profile}]"),
                None => provider.clone(),
            };
            if *success {
                self.report_service_health(principal, workspace, &service, Ok(()));
            } else if let Some(failure) = error.as_deref().and_then(ServiceFailure::from_error) {
                self.report_service_health(principal, workspace, &service, Err(failure));
            }
        }
    }

    pub fn dismiss_service_health_notice(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> bool {
        let Some(RuntimeTransportEvent::HitlRequested { source, .. }) =
            self.pending_hitl_request(principal, workspace, id)
        else {
            return false;
        };
        if source != "service_health" {
            return false;
        }
        let timestamp = {
            let mut notices = self
                .service_health_notices
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            notices.clock = chrono::Utc::now()
                .timestamp_millis()
                .max(notices.clock.saturating_add(1));
            notices.clock
        };
        self.emit_hitl_lifecycle_if_accepted(RuntimeTransportEvent::HitlResolved {
            correlation_id: id.into(),
            source,
            outcome: "dismissed".into(),
            decision: Some("dismiss".into()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.into()),
            workspace: Some(workspace.into()),
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_health_classification_does_not_treat_task_budgets_as_provider_credit() {
        assert_eq!(
            ServiceFailure::from_error("No API key found for the selected model"),
            Some(ServiceFailure::Authentication)
        );
        assert_eq!(
            ServiceFailure::from_error("HTTP 429 insufficient_quota"),
            Some(ServiceFailure::Credit)
        );
        assert_eq!(
            ServiceFailure::from_error("execution token budget exhausted"),
            None
        );
        assert_eq!(ServiceFailure::from_error("invalid action plan"), None);
    }
    #[test]
    fn agy_subscription_quota_emits_one_scoped_rate_limit_notice() {
        use crate::magician_v2::execution::plane::{HarnessStopReason, HarnessTurnSettled};
        let outcome = HarnessTurnSettled {
            assistant_text: "RESOURCE_EXHAUSTED (code 429): Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 29m14s.".into(),
            stop_reason: HarnessStopReason::Refused,
            usage: None,
            native_session_id: None,
        };
        let health = outcome
            .service_health()
            .expect("provider quota is recognized");
        assert_eq!(health, Err(ServiceFailure::RateLimit));
        let bus = RuntimeTransportBroadcaster::new(16);
        let mut events = bus.subscribe();
        for _ in 0..2 {
            bus.report_service_health("alice", "work", "harness:agy", health);
        }
        let RuntimeTransportEvent::HitlRequested {
            principal,
            workspace,
            input_schema,
            prompt,
            ..
        } = events.try_recv().expect("one user notice")
        else {
            panic!("expected HITL notice")
        };
        assert_eq!(principal.as_deref(), Some("alice"));
        assert_eq!(workspace.as_deref(), Some("work"));
        assert_eq!(input_schema.unwrap()["health_issue"], "rate_limit");
        assert!(prompt.contains("Wait for the limit to reset"));
        assert!(!prompt.contains("29m14s"));
        assert!(
            events.try_recv().is_err(),
            "retries must not duplicate the notice"
        );
    }
    #[test]
    fn service_health_pending_notice_survives_restart_and_resolves_on_success() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = super::super::ArtifactV2Workspace::new(temp.path().canonicalize().unwrap());
        let bus =
            RuntimeTransportBroadcaster::new(16).with_hitl_lifecycle_persistence(workspace.clone());
        let mut events = bus.subscribe();
        bus.report_service_health(
            "alice",
            "work",
            "harness:pi",
            Err(ServiceFailure::Authentication),
        );
        let RuntimeTransportEvent::HitlRequested { correlation_id, .. } =
            events.try_recv().unwrap()
        else {
            panic!()
        };
        drop(bus);
        let bus = RuntimeTransportBroadcaster::new(16).with_hitl_lifecycle_persistence(workspace);
        let mut events = bus.subscribe();
        bus.report_service_health(
            "alice",
            "work",
            "harness:pi",
            Err(ServiceFailure::Authentication),
        );
        assert!(
            events.try_recv().is_err(),
            "pending notice must not duplicate after restart"
        );
        bus.report_service_health("alice", "work", "harness:pi", Ok(()));
        assert!(matches!(
            events.try_recv().unwrap(),
            RuntimeTransportEvent::HitlResolved { .. }
        ));
        assert!(bus
            .pending_hitl_request("alice", "work", &correlation_id)
            .is_none());
    }

    #[test]
    fn service_health_notices_are_scoped_deduplicated_and_recoverable() {
        let bus = RuntimeTransportBroadcaster::new(32);
        let mut events = bus.subscribe();
        for _ in 0..5 {
            bus.report_service_health(
                "alice",
                "work",
                "harness-pi",
                Err(ServiceFailure::Authentication),
            );
        }
        let event = events.try_recv().unwrap();
        let RuntimeTransportEvent::HitlRequested {
            correlation_id,
            prompt,
            principal,
            workspace,
            ..
        } = event
        else {
            panic!()
        };
        assert!(prompt.contains("own default model and login"));
        assert_eq!(principal.as_deref(), Some("alice"));
        assert_eq!(workspace.as_deref(), Some("work"));
        assert!(events.try_recv().is_err());
        assert!(!bus.dismiss_service_health_notice("bob", "work", &correlation_id));
        assert!(bus.dismiss_service_health_notice("alice", "work", &correlation_id));
        events.try_recv().unwrap();
        bus.report_service_health(
            "alice",
            "work",
            "harness:pi",
            Err(ServiceFailure::Authentication),
        );
        assert!(
            events.try_recv().is_err(),
            "dismissal suppresses repeated failures"
        );
        bus.report_service_health("alice", "work", "harness:pi", Ok(()));
        bus.report_service_health("alice", "work", "harness:pi", Err(ServiceFailure::Credit));
        assert!(matches!(
            events.try_recv().unwrap(),
            RuntimeTransportEvent::HitlRequested { .. }
        ));
    }
}
