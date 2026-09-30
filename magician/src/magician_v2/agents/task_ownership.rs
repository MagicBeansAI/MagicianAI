//! Which agent owns a newly created task — the vibedev coding.lead override
//! rule. Extracted from `api::task_api_v3` (api-crate extraction
//! prerequisite); the api file re-exports it.

use crate::magician_v2::execution::agent_resources::AgentResources;

/// Who owns a task being created — the one place the VibeDev coding-lead
/// override lives.
///
/// A VibeDev coding *build* run is owned by the config-driven lead engineer
/// (`coding.lead_agent_id`), so swapping the lead (e.g. EM -> CTO) is a one-line
/// config edit rather than a UI/code change. The gate is the SAME canonical
/// signal the executor's coding guardrails use
/// (`is_vibedev_coding_build_run`: `ui_thread_id` OR a `vibedev` tag, but NOT a
/// `plan`/Discuss run), so a plan run keeps the caller-chosen owner.
///
/// A free function rather than a step inside the HTTP handler because the
/// `@vibedev` chat rail creates VibeDev runs too, and it has no `HttpRequest` to
/// reach the handler through. Two copies of this rule would disagree exactly
/// when an operator changed the lead — the moment the override exists for.
pub fn resolve_created_task_owner_agent_id(
    resources: &AgentResources,
    ui_thread_id: &str,
    tags: &[crate::magician_v2::artifact_v2::models::TaskTagRecord],
    requested_agent_id: Option<&str>,
) -> String {
    let is_vibedev_build_run =
        crate::magician_v2::artifact_v2::models::is_vibedev_coding_build_run(ui_thread_id, tags);
    let configured_lead = if is_vibedev_build_run {
        resources
            .coding_lead_agent_id()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    // Make the override auditable: log when the config lead replaces a DIFFERENT
    // caller-sent agent (today they agree, so this stays silent until an operator
    // changes the lead).
    if let (Some(lead), Some(requested)) = (&configured_lead, requested_agent_id) {
        let requested = requested.trim();
        if !requested.is_empty() && requested != lead {
            tracing::info!(
                target: "task_api_v3",
                configured_lead = %lead,
                requested_agent = %requested,
                "vibedev run: overriding client-sent agent_id with coding.lead_agent_id"
            );
        }
    }
    configured_lead
        .or_else(|| requested_agent_id.map(str::to_string))
        .unwrap_or_else(|| "personal-assistant".to_string())
        .trim()
        .to_string()
}
