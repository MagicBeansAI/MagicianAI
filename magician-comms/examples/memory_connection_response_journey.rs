//! Resume a real-model fixture in separate processes to verify durable owner responses.
//! No model calls. Run only on copies of the synthetic live-evaluator fixtures.
use anyhow::{ensure, Context, Result};
use magician::magician_v2::{
    agents::AgentMemoryResolver,
    artifact_v2::workspace::ArtifactV2Workspace,
    attention::resurfacing::{
        memory_connections::*, store::ResurfacingStore, types::FeedbackAction,
    },
    attention_funnel::AttentionScope,
    attention_funnel_store::AttentionFunnelStore,
    feed::FeedStore,
    realtime_events::RuntimeTransportBroadcaster,
    user_requests::{ScopedResponseResult, UserRequestService, UserResponse},
};
use magician_comms::channel_assist::resurfacing::{
    interaction::{MemoryInteractionAdapter, ResurfacingInteractionRegistry},
    memory_connections::ConnectionRuntime,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};

const OWNER_WORDS: &str =
    "For this situation, please use the preference I explicitly confirm next time.";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 5,
        "usage: response_journey FIXTURE_ROOT WORKSPACE CANDIDATE ACTION answer|reconcile"
    );
    let root = PathBuf::from(&args[0]);
    let scope = AttentionScope {
        principal: "memory-eval".into(),
        workspace: args[1].clone(),
    };
    let id = &args[2];
    let action = args[3].as_str();
    let phase = args[4].as_str();
    ensure!(
        matches!(action, "acknowledge" | "remember" | "dismiss" | "stale"),
        "unknown action"
    );
    ensure!(matches!(phase, "answer" | "reconcile"), "unknown phase");
    ensure!(root.is_dir(), "existing synthetic fixture required");
    let layout = ArtifactV2Workspace::new(&root);
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let memory = resolver.resolve_for_scope(&scope.principal, &scope.workspace)?;
    let requests = Arc::new(
        UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
            .with_workspace_layout(layout.clone())
            .with_history_persist_path(root.join("request-history.json"))
            .with_pending_persist_path(root.join("request-pending.json"))
            .await,
    );
    let runtime = ConnectionRuntime {
        store: ResurfacingStore::open(&root)?,
        resolver: resolver.clone(),
        interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(
            MemoryInteractionAdapter::new(resolver),
        )]),
        feed: FeedStore::open_workspace(layout)?,
        requests: Some(requests.clone()),
        router: None,
        attention: Some(AttentionFunnelStore::open(&root)?),
        taste: None,
    };
    let record = runtime
        .store
        .get_connection(&scope.principal, &scope.workspace, id)
        .await?
        .context("missing real-model record")?;
    let hitl = record
        .connection
        .as_ref()
        .is_some_and(|c| c.surface == ConnectionSurface::Hitl);
    let now = chrono::Utc::now().timestamp();
    let report_path = root.join(format!("journey-{phase}.json"));
    ensure!(
        !report_path.exists(),
        "existing journey evidence is immutable"
    );
    let report: Value = if phase == "answer" {
        ensure!(
            record.state == ConnectionState::Published,
            "published real-model record required"
        );
        let before = memory.load_user_knowledge().await?;
        let mut checks = json!({"published_result_restored":true});
        if hitl {
            let pending = requests
                .list_pending_for_scope(&scope.principal, &scope.workspace)
                .await;
            ensure!(
                pending.iter().any(|r| r.id == record.request_id),
                "pending question did not survive model process exit"
            );
            checks["pending_restored"] = json!(true);
            let response = UserResponse {
                request_id: record.request_id.clone(),
                decision: if action == "stale" {
                    "remember".into()
                } else {
                    action.into()
                },
                input: Some(OWNER_WORDS.into()),
                channel: "web".into(),
                sensitive: Vec::new(),
            };
            checks["foreign_scope_rejected"] = json!(
                requests
                    .respond_scoped(
                        response.clone(),
                        Some("foreign-owner"),
                        Some(&scope.workspace)
                    )
                    .await
                    == ScopedResponseResult::ScopeMismatch
            );
            checks["owner_answer_accepted"] = json!(
                requests
                    .respond_scoped(response, Some(&scope.principal), Some(&scope.workspace))
                    .await
                    == ScopedResponseResult::Accepted
            );
        } else {
            ensure!(
                matches!(action, "dismiss" | "stale"),
                "non-HITL journey only supports dismissal or changed evidence"
            );
            if action == "dismiss" {
                runtime
                    .store
                    .record_action(
                        &scope.principal,
                        &scope.workspace,
                        id,
                        FeedbackAction::Dismiss,
                        now,
                        86400,
                        3600,
                    )
                    .await?;
            }
        }
        if action == "stale" {
            let mut changed = before.clone();
            changed["user_preferences"] = json!([]);
            memory.persist_user_knowledge(&changed).await?;
        }
        let before_consumer = memory.load_user_knowledge().await?;
        checks["no_preconsumer_memory_write"] = json!(if action == "stale" {
            before_consumer["user_preferences"] == json!([])
        } else {
            before_consumer == before
        });
        json!({"phase":phase,"action":action,"surface":record.connection.as_ref().map(|c|c.surface),"pid":std::process::id(),"before":before_consumer,"checks":checks})
    } else {
        let answered: Value =
            serde_json::from_slice(&std::fs::read(root.join("journey-answer.json"))?)?;
        ensure!(
            answered["action"] == action,
            "action changed between processes"
        );
        let history = requests.list_history_for_scope(&scope.principal, &scope.workspace, None);
        let response_restored = !hitl
            || history
                .iter()
                .any(|h| h.request.id == record.request_id && h.response.is_some());
        runtime.pass(&scope, &mut None, now).await?;
        let after = memory.load_user_knowledge().await?;
        let completed = runtime
            .store
            .get_connection(&scope.principal, &scope.workspace, id)
            .await?
            .context("missing result after reconciliation")?;
        let mut expected = after.clone();
        let memory_ok = if action == "remember" {
            let fields = after["user_preferences"]
                .as_array()
                .context("missing preferences")?;
            let owner: Vec<_> = fields
                .iter()
                .filter(|p| {
                    p["value"] == OWNER_WORDS && p["source_type"] == "explicit_user_statement"
                })
                .collect();
            let added_key = format!("connection_clarification:{}", record.request_id);
            let remaining: Vec<_> = fields
                .iter()
                .filter(|p| p["key"] != added_key)
                .cloned()
                .collect();
            expected["user_preferences"] = json!(remaining);
            owner.len() == 1 && owner[0]["key"] == added_key && expected == answered["before"]
        } else {
            after == answered["before"]
        };
        runtime.pass(&scope, &mut None, now + 1).await?;
        let repeated = runtime
            .store
            .get_connection(&scope.principal, &scope.workspace, id)
            .await?
            .context("missing repeated result")?;
        let feedback = runtime
            .store
            .list_feedback_attention_repairs_after(&scope.principal, &scope.workspace, 0, 0, 20)
            .await?;
        let pending = requests
            .list_pending_for_scope(&scope.principal, &scope.workspace)
            .await;
        let feed = runtime
            .feed
            .get_item(&scope.principal, &scope.workspace, &record.feed_id)
            .await?;
        let phrasing = runtime
            .store
            .get_phrasing(&scope.principal, &scope.workspace, id)
            .await?;
        json!({"phase":phase,"action":action,"pid":std::process::id(),"state":completed.state,"after":after,
            "checks":{"new_process":answered["pid"]!=std::process::id(),"answer_restored":response_restored,
            "only_explicit_owner_memory_written":memory_ok,"terminal_state":completed.state==if action=="stale" || !hitl{ConnectionState::Withdrawn}else{ConnectionState::Done},
            "idempotent_reconciliation":memory.load_user_knowledge().await?==after && repeated.state==completed.state,
            "feedback_not_duplicated":feedback.len()<=1,"no_pending_question":pending.is_empty(),
            "dismissed_or_stale_delivery_absent":if matches!(action,"dismiss"|"stale"){feed.is_none() && phrasing.is_none()}else{true}}})
    };
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", report_path.display());
    ensure!(
        report["checks"]
            .as_object()
            .context("checks missing")?
            .values()
            .all(|v| v == true),
        "journey checks failed"
    );
    Ok(())
}
