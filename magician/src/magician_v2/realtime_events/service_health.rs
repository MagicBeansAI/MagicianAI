//! Scoped health notices on the existing durable HITL lifecycle.
//! Raw provider errors may contain secrets or request bodies and never enter a
//! notice. A harness CLI's failure may add a [`ServiceDetail`]: one line of the
//! CLI's own reason, already bounded and secrets-redacted
//! (`harness_auth::failure::bounded_reason`), and a remedy.
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
            "402 payment",
            "insufficient_credits",
            "insufficient balance",
            "exceeded your current quota",
            "quota exceeded",
            "no active subscription",
            "subscription required",
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
            "session has expired",
            "session expired",
            "refreshtokenrejected",
            "refresh token rejected",
            "invalid_grant",
            "token expired",
            "token has expired",
            "please log in",
            "please login",
            "log in again",
            "sign in again",
            "401 unauthorized",
            "403 forbidden",
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
            "usage limit",
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
            "failed to lookup address",
            "dns error",
            "network is unreachable",
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
    /// The failure class's wire name (`authentication`, `credit`,
    /// `rate_limit`, `unavailable`).
    pub fn code(self) -> &'static str {
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

/// What a harness CLI said about its own failure, for the notice: a title
/// (`Grok`), one bounded, redacted line of its reason, and a remedy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDetail {
    pub title: String,
    pub reason: String,
    pub remedy: String,
}

impl ServiceDetail {
    fn prompt(&self) -> String {
        let mut reason = self.reason.trim().to_string();
        if !reason.ends_with(['.', '!', '?', '…']) {
            reason.push('.');
        }
        format!("{}: {reason} {}", self.title, self.remedy)
    }
}

#[derive(Clone)]
struct Incident {
    id: String,
    failure: ServiceFailure,
    last_seen: i64,
}
/// Model-catalog drift notices per scope: the last drift revision a scope
/// was reconciled against, and the notice raised per source.
#[derive(Default)]
struct CatalogDriftNotices {
    seen_revision: HashMap<(String, String), u64>,
    raised: HashMap<(String, String, String), String>,
}

/// A scope whose derived memory index only an explicit rebuild can repair
/// (the runtime maintainer is merge-only and never replaces `lancedb/`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryIndexRebuildNotice {
    /// Stable per incident (reason plus the stale manifest's identity): a
    /// dismissed incident is not raised again, a later one is.
    pub incident: String,
    /// The stale reason, e.g. `schema_version_changed`.
    pub reason: String,
    /// One readable line, e.g. `the index has schema 9; this build needs 11`.
    pub detail: String,
    /// The exact operator command that repairs it.
    pub command: String,
}

fn memory_index_rebuild_notice_id(incident: &str) -> String {
    format!(
        "service_health:memory_index_rebuild:{:x}",
        Sha256::digest(incident.as_bytes())
    )
}

#[derive(Default)]
pub(super) struct HealthNotices {
    catalog_drift: CatalogDriftNotices,
    /// Raised memory-index rebuild notice per scope.
    memory_index_rebuild: HashMap<(String, String), String>,
    active: HashMap<(String, String, String), Incident>,
    /// First unanswered `Unavailable` per service, while it is inside
    /// `UNAVAILABLE_GRACE_MS`. A success clears it.
    unavailable_since: HashMap<(String, String, String), i64>,
    clock: i64,
}

/// How long a service must stay unreachable (no success in between) before
/// the reader is told. A single timeout is routine: a local model's one-slot
/// queue under a burst of background decisions times out, the route falls back
/// and the work completes, and the next call succeeds. Raising and clearing a
/// notice for each of those (Kev-4B every ~10 minutes) was noise; a service
/// that is really down keeps failing and is reported once the window passes.
/// Authentication, credit and rate-limit problems are reported at once.
const UNAVAILABLE_GRACE_MS: i64 = 120_000;

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
        self.report_service_health_with_detail(principal, workspace, service, result, None);
    }

    /// [`Self::report_service_health`] whose notice, when one is raised,
    /// says what the failing CLI said and what to do (`detail`). Still one
    /// notice per scoped service and failure class.
    pub fn report_service_health_with_detail(
        &self,
        principal: &str,
        workspace: &str,
        service: &str,
        result: Result<(), ServiceFailure>,
        detail: Option<&ServiceDetail>,
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
            notices.unavailable_since.remove(&key);
            return;
        };
        if failure == ServiceFailure::Unavailable {
            if notices.unavailable_since.len() >= 1024
                && !notices.unavailable_since.contains_key(&key)
            {
                notices.unavailable_since.clear();
            }
            let since = *notices.unavailable_since.entry(key.clone()).or_insert(now);
            if now.saturating_sub(since) < UNAVAILABLE_GRACE_MS {
                return;
            }
        }
        notices.unavailable_since.remove(&key);
        let mut schema = json!({"type":"choice","options":[{"id":"dismiss","label":"Dismiss"}],"allow_other":false,"service":service,"health_issue":failure.code()});
        if let Some(detail) = detail {
            schema["reason"] = json!(detail.reason);
            schema["remedy"] = json!(detail.remedy);
        }
        let event=RuntimeTransportEvent::HitlRequested {
            correlation_id:id.clone(),source:"service_health".into(),input_type:"choice".into(),
            prompt:detail.map(ServiceDetail::prompt).unwrap_or_else(|| failure.guidance(&service)),hint:Some("This notice does not change engine settings or retry completed work. It clears after a successful call, or you can dismiss it.".into()),
            input_schema:Some(schema),
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
            // Catalog drift is instance-wide; an active scope learns of it on
            // its next model call. One atomic read when nothing changed.
            let revision = magicllm::model_discovery::drift_revision();
            if revision > 0 {
                let seen = self
                    .service_health_notices
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .catalog_drift
                    .seen_revision
                    .get(&(principal.clone(), workspace.clone()))
                    .copied();
                if seen != Some(revision) {
                    let (revision, drift) = magicllm::model_discovery::published_drift();
                    self.report_model_catalog_drift(principal, workspace, revision, &drift);
                }
            }
        }
    }

    /// One Attention notice per drifted model source in this scope: catalogued
    /// models the source no longer offers and offered models the catalog
    /// lacks. A changed drift replaces the source's notice; a source that no
    /// longer drifts resolves it; a dismissed drift is not raised again.
    pub fn report_model_catalog_drift(
        &self,
        principal: &str,
        workspace: &str,
        revision: u64,
        drift: &[magicllm::model_discovery::SourceDrift],
    ) {
        if principal.is_empty() || workspace.is_empty() {
            return;
        }
        let scope = (principal.to_owned(), workspace.to_owned());
        let mut notices = self
            .service_health_notices
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if notices.catalog_drift.seen_revision.get(&scope) == Some(&revision) {
            return;
        }
        if notices.catalog_drift.seen_revision.len() >= 1024 {
            notices.catalog_drift.seen_revision.clear();
        }
        notices
            .catalog_drift
            .seen_revision
            .insert(scope.clone(), revision);
        let mut now = chrono::Utc::now()
            .timestamp_millis()
            .max(notices.clock.saturating_add(1));
        let mut sources: Vec<String> = notices
            .catalog_drift
            .raised
            .keys()
            .filter(|(p, w, _)| *p == scope.0 && *w == scope.1)
            .map(|(_, _, source)| source.clone())
            .collect();
        sources.extend(drift.iter().map(|entry| entry.source.clone()));
        sources.sort();
        sources.dedup();
        for source in sources {
            let key = (scope.0.clone(), scope.1.clone(), source.clone());
            let current = drift.iter().find(|entry| entry.source == source);
            let id = current.map(|entry| {
                format!(
                    "service_health:model_catalog:{:x}",
                    Sha256::digest(entry.fingerprint().as_bytes())
                )
            });
            let previous = notices.catalog_drift.raised.get(&key).cloned();
            if previous.is_some() && previous == id {
                continue;
            }
            if let Some(previous) = previous {
                if self
                    .pending_hitl_request(principal, workspace, &previous)
                    .is_some()
                {
                    now = now.saturating_add(1);
                    self.emit_hitl_lifecycle_if_accepted(RuntimeTransportEvent::HitlResolved {
                        correlation_id: previous,
                        source: "service_health".into(),
                        outcome: "responded".into(),
                        decision: Some("catalog_changed".into()),
                        task_id: None,
                        execution_id: None,
                        agent_id: None,
                        principal: Some(principal.into()),
                        workspace: Some(workspace.into()),
                        timestamp: now,
                    });
                }
                notices.catalog_drift.raised.remove(&key);
            }
            let (Some(entry), Some(id)) = (current, id) else {
                continue;
            };
            match self.hitl_lifecycle_state(principal, workspace, &id) {
                // Already raised (e.g. before a restart), or dismissed.
                super::HitlLifecycleState::Pending(_) | super::HitlLifecycleState::Resolved => {
                    notices.catalog_drift.raised.insert(key, id);
                    continue;
                },
                _ => {},
            }
            let label = source.replace("harness-", "harness:");
            now = now.saturating_add(1);
            let event = RuntimeTransportEvent::HitlRequested {
                correlation_id: id.clone(),
                source: "service_health".into(),
                input_type: "choice".into(),
                prompt: catalog_drift_prompt(&label, entry),
                hint: Some("Unavailable models are hidden from model pickers; uncatalogued models use the default request shape and have no price yet. This notice changes no settings.".into()),
                input_schema: Some(json!({
                    "type": "choice",
                    "options": [{"id": "dismiss", "label": "Dismiss"}],
                    "allow_other": false,
                    "service": label,
                    "health_issue": "model_catalog_drift",
                    "missing": entry.missing_models.iter().take(50).collect::<Vec<_>>(),
                    "uncatalogued": entry.uncatalogued.iter().take(50).collect::<Vec<_>>(),
                })),
                task_id: None,
                execution_id: None,
                agent_id: None,
                principal: Some(principal.into()),
                workspace: Some(workspace.into()),
                timestamp: now,
            };
            if self.emit_hitl_lifecycle_if_accepted(event) {
                if notices.catalog_drift.raised.len() >= 1024 {
                    notices.catalog_drift.raised.clear();
                }
                notices.catalog_drift.raised.insert(key, id);
            }
        }
        notices.clock = now;
    }

    /// One Attention notice per scope saying its memory index needs an
    /// explicit rebuild, with the command. Idempotent: a raised or dismissed
    /// notice for the same incident is not raised again (also after restart).
    pub fn raise_memory_index_rebuild_notice(
        &self,
        principal: &str,
        workspace: &str,
        notice: &MemoryIndexRebuildNotice,
    ) -> bool {
        if principal.is_empty() || workspace.is_empty() {
            return false;
        }
        let key = (principal.to_owned(), workspace.to_owned());
        let id = memory_index_rebuild_notice_id(&notice.incident);
        let mut notices = self
            .service_health_notices
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if notices.memory_index_rebuild.get(&key) == Some(&id) {
            return false;
        }
        if self
            .pending_hitl_request(principal, workspace, &id)
            .is_some()
            || matches!(
                self.hitl_lifecycle_state(principal, workspace, &id),
                super::HitlLifecycleState::Pending(_) | super::HitlLifecycleState::Resolved
            )
        {
            if notices.memory_index_rebuild.len() >= 1024 {
                notices.memory_index_rebuild.clear();
            }
            notices.memory_index_rebuild.insert(key, id);
            return false;
        }
        let now = chrono::Utc::now()
            .timestamp_millis()
            .max(notices.clock.saturating_add(1));
        notices.clock = now;
        let event = RuntimeTransportEvent::HitlRequested {
            correlation_id: id.clone(),
            source: "service_health".into(),
            input_type: "choice".into(),
            prompt: format!(
                "The memory search index for this workspace needs a rebuild: {}. Memory search uses direct ranking until then; no memory is lost. Run `{}` (or `make memory-index-rebuild` with MEMORY_INDEX_PRINCIPAL and MEMORY_INDEX_WORKSPACE set to this scope).",
                notice.detail.trim_end_matches('.'),
                notice.command
            ),
            hint: Some("Runtime maintenance never replaces the index on its own and has stopped retrying. This notice clears after a successful rebuild, or you can dismiss it.".into()),
            input_schema: Some(json!({
                "type": "choice",
                "options": [{"id": "dismiss", "label": "Dismiss"}],
                "allow_other": false,
                "service": "memory_index",
                "health_issue": "memory_index_rebuild_required",
                "reason": notice.reason,
                "command": notice.command,
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.into()),
            workspace: Some(workspace.into()),
            timestamp: now,
        };
        if !self.emit_hitl_lifecycle_if_accepted(event) {
            return false;
        }
        if notices.memory_index_rebuild.len() >= 1024 {
            notices.memory_index_rebuild.clear();
        }
        notices.memory_index_rebuild.insert(key, id);
        true
    }

    /// Resolve the scope's memory-index rebuild notice for `incident` once a
    /// rebuild succeeded (or the condition no longer holds).
    pub fn resolve_memory_index_rebuild_notice(
        &self,
        principal: &str,
        workspace: &str,
        incident: &str,
    ) -> bool {
        let key = (principal.to_owned(), workspace.to_owned());
        let id = memory_index_rebuild_notice_id(incident);
        let mut notices = self
            .service_health_notices
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if notices.memory_index_rebuild.get(&key) == Some(&id) {
            notices.memory_index_rebuild.remove(&key);
        }
        if self
            .pending_hitl_request(principal, workspace, &id)
            .is_none()
        {
            return false;
        }
        let now = chrono::Utc::now()
            .timestamp_millis()
            .max(notices.clock.saturating_add(1));
        notices.clock = now;
        self.emit_hitl_lifecycle_if_accepted(RuntimeTransportEvent::HitlResolved {
            correlation_id: id,
            source: "service_health".into(),
            outcome: "responded".into(),
            decision: Some("memory_index_rebuilt".into()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.into()),
            workspace: Some(workspace.into()),
            timestamp: now,
        })
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

fn name_list(names: &[String]) -> String {
    const SHOWN: usize = 6;
    let mut text = names
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    text
}

fn catalog_drift_prompt(service: &str, drift: &magicllm::model_discovery::SourceDrift) -> String {
    let mut parts = Vec::new();
    if !drift.missing_models.is_empty() {
        parts.push(format!(
            "it no longer offers {} catalogued model{} ({})",
            drift.missing_models.len(),
            if drift.missing_models.len() == 1 {
                ""
            } else {
                "s"
            },
            name_list(&drift.missing_models)
        ));
    }
    if !drift.uncatalogued.is_empty() {
        parts.push(format!(
            "it offers {} model{} the catalog does not list ({})",
            drift.uncatalogued.len(),
            if drift.uncatalogued.len() == 1 {
                ""
            } else {
                "s"
            },
            name_list(&drift.uncatalogued)
        ));
    }
    format!(
        "The model catalog is out of date for {service}: {}. Update the catalog (llm-router.yaml) to add prices and request shapes, or remove withdrawn models.",
        parts.join("; ")
    )
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
    fn a_harness_failure_notice_carries_the_cli_reason_and_remedy_once() {
        let bus = RuntimeTransportBroadcaster::new(16);
        let mut events = bus.subscribe();
        let detail = ServiceDetail {
            title: "Grok".into(),
            reason: "Your session has expired. Run `grok login` to sign in again.".into(),
            remedy: "Run `grok login`.".into(),
        };
        // Two model labels used to be two services; the engine is one.
        for _ in 0..2 {
            bus.report_service_health_with_detail(
                "alice",
                "work",
                "harness:grok",
                Err(ServiceFailure::Authentication),
                Some(&detail),
            );
        }
        let Ok(RuntimeTransportEvent::HitlRequested {
            prompt,
            input_schema,
            ..
        }) = events.try_recv()
        else {
            panic!("expected one notice");
        };
        assert!(
            prompt.starts_with("Grok: Your session has expired."),
            "{prompt}"
        );
        assert!(prompt.ends_with("Run `grok login`."));
        let schema = input_schema.unwrap();
        assert_eq!(schema["remedy"], "Run `grok login`.");
        assert_eq!(schema["health_issue"], "authentication");
        assert!(
            events.try_recv().is_err(),
            "one notice per engine and class"
        );
    }

    #[test]
    fn expired_sessions_quota_and_dns_failures_are_classified() {
        for (text, expected) in [
            (
                "Your session has expired. Run `grok login` to sign in again.",
                ServiceFailure::Authentication,
            ),
            (
                "auth.refresh.permanent_failure RefreshTokenRejected",
                ServiceFailure::Authentication,
            ),
            (
                "You exceeded your current quota, please check your plan",
                ServiceFailure::Credit,
            ),
            (
                "Claude AI usage limit reached|1760000000",
                ServiceFailure::RateLimit,
            ),
            (
                "Reconnecting... 2/5 (failed to lookup address information)",
                ServiceFailure::Unavailable,
            ),
        ] {
            assert_eq!(ServiceFailure::from_error(text), Some(expected), "{text}");
        }
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
    fn a_passing_timeout_raises_no_notice_but_a_sustained_one_does() {
        let bus = RuntimeTransportBroadcaster::new(32);
        let mut events = bus.subscribe();
        let service = "Decision model:kev-4b";

        // One timeout, then a success: routine, nothing reaches the reader.
        bus.report_service_health("alice", "work", service, Err(ServiceFailure::Unavailable));
        bus.report_service_health("alice", "work", service, Ok(()));
        bus.report_service_health("alice", "work", service, Err(ServiceFailure::Unavailable));
        assert!(events.try_recv().is_err());

        // Still failing once the grace window has passed with no success.
        {
            let mut notices = bus.service_health_notices.lock().unwrap();
            for since in notices.unavailable_since.values_mut() {
                *since -= UNAVAILABLE_GRACE_MS + 1;
            }
        }
        bus.report_service_health("alice", "work", service, Err(ServiceFailure::Unavailable));
        let Ok(RuntimeTransportEvent::HitlRequested { prompt, .. }) = events.try_recv() else {
            panic!("a sustained outage must be reported");
        };
        assert!(prompt.contains("unreachable or timed out"));
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

    #[test]
    fn catalog_drift_raises_one_notice_per_source_and_resolves_when_fixed() {
        use magicllm::model_discovery::SourceDrift;
        let bus = RuntimeTransportBroadcaster::new(32);
        let mut events = bus.subscribe();
        let drift = vec![SourceDrift {
            source: "harness-agy".into(),
            missing: vec!["harness-agy/gemini-3.7-flash-high".into()],
            missing_models: vec!["gemini-3.7-flash-high".into()],
            uncatalogued: vec!["gemini-3.8-flash-high".into()],
        }];
        bus.report_model_catalog_drift("drift-owner", "work", 1, &drift);
        bus.report_model_catalog_drift("drift-owner", "work", 1, &drift);
        // A new revision with the same drift for this source is not a new notice.
        bus.report_model_catalog_drift("drift-owner", "work", 2, &drift);
        let RuntimeTransportEvent::HitlRequested {
            prompt,
            input_schema,
            source,
            ..
        } = events.try_recv().expect("one drift notice")
        else {
            panic!("expected a HITL notice")
        };
        assert_eq!(source, "service_health");
        assert!(prompt.contains("harness:agy"));
        assert!(prompt.contains("gemini-3.7-flash-high"));
        assert!(prompt.contains("gemini-3.8-flash-high"));
        let schema = input_schema.unwrap();
        assert_eq!(schema["health_issue"], "model_catalog_drift");
        assert_eq!(schema["missing"], json!(["gemini-3.7-flash-high"]));
        assert!(events.try_recv().is_err(), "no duplicate notice");
        // Another scope gets its own notice.
        bus.report_model_catalog_drift("drift-owner", "home", 2, &drift);
        assert!(matches!(
            events.try_recv().unwrap(),
            RuntimeTransportEvent::HitlRequested { .. }
        ));
        // The catalog caught up: the source's notice resolves.
        bus.report_model_catalog_drift("drift-owner", "work", 3, &[]);
        assert!(matches!(
            events.try_recv().unwrap(),
            RuntimeTransportEvent::HitlResolved { .. }
        ));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn memory_index_rebuild_notice_is_raised_once_per_scope_and_resolves_after_rebuild() {
        let bus = RuntimeTransportBroadcaster::new(32);
        let mut events = bus.subscribe();
        let notice = MemoryIndexRebuildNotice {
            incident: "schema_version_changed:2026-10-06T12:30:19Z".into(),
            reason: "schema_version_changed".into(),
            detail: "the index has schema 9; this build needs 11".into(),
            command:
                "./magician.bin memory-index rebuild --principal anonymous --workspace default"
                    .into(),
        };
        assert!(bus.raise_memory_index_rebuild_notice("anonymous", "default", &notice));
        assert!(!bus.raise_memory_index_rebuild_notice("anonymous", "default", &notice));
        let RuntimeTransportEvent::HitlRequested {
            prompt,
            input_schema,
            source,
            ..
        } = events.try_recv().expect("one rebuild notice")
        else {
            panic!("expected a HITL notice")
        };
        assert_eq!(source, "service_health");
        assert!(prompt.contains("schema 9"));
        assert!(prompt.contains("memory-index rebuild --principal anonymous --workspace default"));
        let schema = input_schema.unwrap();
        assert_eq!(schema["health_issue"], "memory_index_rebuild_required");
        assert_eq!(schema["reason"], "schema_version_changed");
        assert!(events.try_recv().is_err(), "no duplicate notice");

        assert!(bus.resolve_memory_index_rebuild_notice("anonymous", "default", &notice.incident));
        assert!(matches!(
            events.try_recv().unwrap(),
            RuntimeTransportEvent::HitlResolved { .. }
        ));
        assert!(!bus.resolve_memory_index_rebuild_notice("anonymous", "default", &notice.incident));
    }

    #[test]
    fn catalog_drift_prompt_caps_long_name_lists() {
        let drift = magicllm::model_discovery::SourceDrift {
            source: "openai".into(),
            missing: vec![],
            missing_models: vec![],
            uncatalogued: (0..10).map(|n| format!("model-{n}")).collect(),
        };
        let prompt = catalog_drift_prompt("openai", &drift);
        assert!(prompt.contains("offers 10 models"));
        assert!(prompt.contains("and 4 more"));
        assert!(!prompt.contains("model-9"));
    }
}
