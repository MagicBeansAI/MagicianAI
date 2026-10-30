//! Live wiring of the pure tier-distill logic ([`super::tier_distill`]).
//!
//! [`distill_tier_producer`] loads a producer's roll-up rows from scoped user
//! memory, runs the cluster → salience → distil → stamp path, routes the
//! resulting records through the evidence→memory bridge, and appends them to the
//! work-evidence store. It is the ONE routine shared by the HTTP entrypoint
//! (`POST /evidence/distill/{producer}`) and the `distill_evidence` tool the
//! scheduled observe writers call as their final step — so the trigger surface
//! (HTTP vs. tool) is just a thin scope-resolution shell over this.

use serde_json::{json, Value};

use super::tier_distill::distill_tier_cluster_with_source;
use super::{
    cluster_tier_entries, entity_candidates_from_evidence, is_salient, is_tier_cluster_salient,
    producer_spec, stamp_tier_evidence,
};
use crate::magician_v2::agents::memory::AgentMemoryResolver;
use crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use std::sync::Arc;

/// Distil every salient `(account, day)` cluster in `producer`'s tier into
/// work-evidence for the given scope. Returns a `{rows, clusters, promotable,
/// distilled}` summary on success, or a human-readable error string. Idempotent:
/// re-running upserts the same deterministic `evidence_id`s, so a missed cadence
/// self-heals on the next run.
#[allow(clippy::too_many_arguments)]
pub async fn distill_tier_producer(
    memory_resolver: &AgentMemoryResolver,
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    producer: &str,
    days: u64,
    router: &OperationLlmRouter,
    prompt_manager: &Arc<PromptManager>,
) -> Result<Value, String> {
    distill_tier_producer_with_broadcaster(
        memory_resolver,
        workspace_layout,
        principal,
        workspace,
        producer,
        days,
        router,
        prompt_manager,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn distill_tier_producer_with_broadcaster(
    memory_resolver: &AgentMemoryResolver,
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    producer: &str,
    days: u64,
    router: &OperationLlmRouter,
    prompt_manager: &Arc<PromptManager>,
    event_broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
) -> Result<Value, String> {
    let spec =
        producer_spec(producer).ok_or_else(|| format!("unknown evidence producer: {producer}"))?;
    let days = days.max(1);
    let since_day = (chrono::Utc::now() - chrono::Duration::days(days as i64))
        .format("%Y-%m-%d")
        .to_string();

    let memory = memory_resolver
        .resolve_for_scope(principal, workspace)
        .map_err(|err| err.to_string())?;
    let settings = crate::magician_v2::agents::user_memory_bounds::current_settings();
    let entries: Vec<Value> = load_producer_rows(&memory, &spec, &settings)
        .await?
        .into_iter()
        .filter(|e| {
            e.get("day")
                .or_else(|| e.get("date"))
                .and_then(|d| d.as_str())
                .map(|d| d >= since_day.as_str())
                .unwrap_or(true)
        })
        .collect();

    let clusters = cluster_tier_entries(&entries, &spec);
    if clusters.is_empty() {
        return Ok(json!({
            "producer": spec.producer,
            "rows": entries.len(),
            "clusters": 0,
            "promotable": 0,
            "distilled": 0,
        }));
    }
    let promotable: Vec<_> = clusters
        .iter()
        .filter(|c| is_tier_cluster_salient(c, &spec))
        .cloned()
        .collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut distilled = 0usize;
    let telemetry = event_broadcaster.map(|broadcaster| {
        OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            principal,
            workspace,
            "evidence_distillation",
        )
    });
    let scoped_router =
        router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));

    for cluster in &promotable {
        let proposal = match distill_tier_cluster_with_source(
            cluster,
            &spec,
            &scoped_router,
            prompt_manager,
            telemetry.as_ref(),
            Some((&memory, prompt_manager)),
        )
        .await
        {
            Ok(proposal) => proposal,
            Err(err) => {
                tracing::warn!(producer = spec.producer, error = %err, "tier distill failed (skipped)");
                continue;
            },
        };
        let Some(record) = stamp_tier_evidence(&proposal, cluster, &spec, &now) else {
            continue;
        };
        if !is_salient(&record) {
            continue;
        }
        let mut candidates = entity_candidates_from_evidence(&record);
        for c in &mut candidates {
            c.producer = spec.producer.to_string();
        }
        let learning_scope = crate::magician_v2::learning::LearningScope::new(
            principal.to_string(),
            workspace.to_string(),
        );
        if let Err(err) = crate::magician_v2::learning::route_user_evidence_to_memory(
            workspace_layout,
            &learning_scope,
            &record,
        )
        .await
        {
            tracing::warn!(
                evidence_id = %record.evidence_id,
                error = %err,
                "tier evidence → memory candidate route failed (non-fatal, skipped)"
            );
        }
        memory
            .append_user_work_evidence(record)
            .await
            .map_err(|err| err.to_string())?;
        if !candidates.is_empty() {
            let _ = memory.resolve_user_work_entities(candidates).await;
        }
        distilled += 1;
    }

    Ok(json!({
        "producer": spec.producer,
        "rows": entries.len(),
        "clusters": clusters.len(),
        "promotable": promotable.len(),
        "distilled": distilled,
    }))
}

/// A producer's roll-up rows.
///
/// The screen, meeting and calendar writers normalise their tier name, so
/// their rows sit at the top level of `knowledge.json`. The channel bridges
/// write `user.email_evidence` / `user.chat_evidence` unnormalised, so those
/// rows sit under the nested `user` object — or, under
/// `memory.user_knowledge_bounds` enforce, in their lane files. This reader
/// used to look only at the top level and found none of them. It reads the
/// nested and lane rows only when `lanes.distill_enabled` is on: that is off
/// until the owner agrees to email and chat feeding work evidence (model
/// calls and new memory suggestions), so by default nothing changes.
pub async fn load_producer_rows(
    memory: &crate::magician_v2::agents::AgentMemoryService,
    spec: &super::TierProducerSpec,
    settings: &crate::magician_v2::agents::user_memory_bounds::MagicianMemoryBoundsSettings,
) -> Result<Vec<Value>, String> {
    let knowledge = memory
        .load_user_knowledge()
        .await
        .map_err(|err| err.to_string())?;
    let tier = crate::magician_v2::chat::service::normalized_user_memory_tier_name(spec.tier)
        .unwrap_or_default();
    let mut rows: Vec<Value> = knowledge
        .get(tier.as_str())
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(lane) = crate::magician_v2::agents::user_memory_lanes::Lane::for_tier(spec.tier)
    else {
        return Ok(rows);
    };
    if !settings.lanes.distill_enabled {
        return Ok(rows);
    }
    if let Some(nested) = knowledge
        .get("user")
        .and_then(|user| user.get(tier.as_str()))
        .and_then(Value::as_array)
    {
        rows.extend(nested.iter().cloned());
    }
    rows.extend(
        crate::magician_v2::agents::user_memory_lanes::load_lane(memory.storage(), lane)
            .await
            .map_err(|err| err.to_string())?,
    );
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::user_memory_bounds::{
        with_settings, MagicianMemoryBoundsSettings, UserKnowledgeBoundsMode,
    };
    use crate::magician_v2::chat::service::merge_user_memory_tier_fields;

    fn email_row() -> serde_json::Map<String, Value> {
        serde_json::Map::from_iter([(
            "email:work:2026-10-17".to_string(),
            json!({"account":"work","day":"2026-10-17","source_type":"email_capture",
                "summary":"2 email threads on 2026-10-17 for work","thread_count":2}),
        )])
    }

    /// Finding F3 of the growth-bounds plan: the evidence bridge's write
    /// lands under `knowledge.user`, where this reader never looked. With
    /// distillation from the activity logs off (the default) that stays so;
    /// switched on, the reader finds the row in either store.
    #[tokio::test]
    async fn the_distiller_finds_bridge_rows_only_when_switched_on() {
        let root = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(root.path());
        let service = resolver.resolve_for_scope("p", "w").unwrap();
        let spec = super::super::producer_spec("email").unwrap();
        let outcome = merge_user_memory_tier_fields(
            &resolver,
            "p",
            "w",
            crate::magician_v2::evidence::tier_contracts::EMAIL_EVIDENCE_TIER,
            &email_row(),
        )
        .await;
        assert_eq!(outcome["status"], "ok");
        let off = MagicianMemoryBoundsSettings::default();
        assert!(load_producer_rows(&service, &spec, &off)
            .await
            .unwrap()
            .is_empty());
        let mut on = off.clone();
        on.lanes.distill_enabled = true;
        assert_eq!(
            load_producer_rows(&service, &spec, &on)
                .await
                .unwrap()
                .len(),
            1
        );

        // Under enforce the bridge's write goes to the lane file instead.
        let lane_root = tempfile::tempdir().unwrap();
        let lane_resolver = AgentMemoryResolver::new(lane_root.path());
        let lane_service = lane_resolver.resolve_for_scope("p", "w").unwrap();
        let mut enforce = on.clone();
        enforce.mode = UserKnowledgeBoundsMode::Enforce;
        let outcome = with_settings(enforce.clone(), async {
            merge_user_memory_tier_fields(
                &lane_resolver,
                "p",
                "w",
                crate::magician_v2::evidence::tier_contracts::EMAIL_EVIDENCE_TIER,
                &email_row(),
            )
            .await
        })
        .await;
        assert_eq!(outcome["store"], "lane");
        let knowledge = lane_service.load_user_knowledge().await.unwrap();
        assert!(knowledge.get("user").is_none());
        let rows = load_producer_rows(&lane_service, &spec, &enforce)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(cluster_tier_entries(&rows, &spec).len(), 1);
    }
}
