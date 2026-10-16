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
/// A plan run that names no owner gets the lead too, never the generic
/// `personal-assistant`: a Discuss run's one plan call is a `run_coding_task`
/// the lead delegates to an engineer, and the assistant has neither the tool
/// nor an engineer to delegate to, so its Discuss runs could never plan.
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
    let is_vibedev_run =
        crate::magician_v2::artifact_v2::models::is_vibedev_cockpit_run(ui_thread_id, tags);
    let lead = (is_vibedev_build_run || is_vibedev_run)
        .then(|| resources.coding_lead_agent_id())
        .flatten();
    // Make the override auditable: log when the config lead replaces a DIFFERENT
    // caller-sent agent (today they agree, so this stays silent until an operator
    // changes the lead).
    if let (true, Some(lead), Some(requested)) = (is_vibedev_build_run, lead.as_deref(), requested_agent_id) {
        let (lead, requested) = (lead.trim(), requested.trim());
        if !lead.is_empty() && !requested.is_empty() && requested != lead {
            tracing::info!(
                target: "task_api_v3",
                configured_lead = %lead,
                requested_agent = %requested,
                "vibedev run: overriding client-sent agent_id with coding.lead_agent_id"
            );
        }
    }
    choose_owner(lead.as_deref(), is_vibedev_build_run, is_vibedev_run, requested_agent_id)
}

/// The owner rule itself: a build run's lead wins; otherwise the caller's
/// choice; a VibeDev run with none falls to the lead; anything else to the
/// generic assistant.
fn choose_owner(
    lead: Option<&str>,
    is_vibedev_build_run: bool,
    is_vibedev_run: bool,
    requested_agent_id: Option<&str>,
) -> String {
    let lead = lead.map(str::trim).filter(|lead| !lead.is_empty());
    let requested = requested_agent_id.map(str::trim).filter(|agent| !agent.is_empty());
    let owner = if is_vibedev_build_run {
        lead.or(requested)
    } else if is_vibedev_run {
        requested.or(lead)
    } else {
        requested
    };
    owner.unwrap_or("personal-assistant").to_string()
}

#[cfg(test)]
mod tests {
    use super::choose_owner;

    #[test]
    fn a_build_run_is_the_leads_and_a_discuss_run_keeps_the_callers_or_gets_the_lead() {
        // Build: the lead overrides the caller.
        assert_eq!(choose_owner(Some("cto"), true, true, Some("engineering-manager")), "cto");
        assert_eq!(choose_owner(None, true, true, Some("engineering-manager")), "engineering-manager");
        // Discuss: the caller's choice stands; with none, the lead, never the assistant.
        assert_eq!(choose_owner(Some("cto"), false, true, Some("engineering-manager")), "engineering-manager");
        assert_eq!(choose_owner(Some("cto"), false, true, None), "cto");
        assert_eq!(choose_owner(Some("cto"), false, true, Some("  ")), "cto");
        assert_eq!(choose_owner(None, false, true, None), "personal-assistant");
        // Not VibeDev: the caller's, else the assistant; the lead never applies.
        assert_eq!(choose_owner(Some("cto"), false, false, None), "personal-assistant");
        assert_eq!(choose_owner(Some("cto"), false, false, Some("researcher")), "researcher");
    }
}
