//! §4.2c — the adversarial proof matrix for the engagement authority carrier.
//!
//! This binary is deliberately separate from the crate's unit tests. The
//! dispatch boundary in `execute_action_inner` calls the **global** wrappers
//! `authorize_engagement_dispatch` / `authorize_engagement_delegation`, so
//! proving the real path requires a store installed in the process-wide
//! `OnceLock`. The unit-test binary asserts the opposite — that no store is
//! installed, which is how row 8 (fail closed on store absence) is proven —
//! and installing one there would make that test order-dependent. One process,
//! one truth about the global: hence two binaries.
//!
//! What this harness proves, precisely: the authority decisions that the
//! executor makes are taken against a live store through the same global
//! entry points the executor uses, the carrier survives the transports that
//! could drop it, and — since 2026-08-18, row 1a — a **real agentic execution**
//! is stopped at the dispatch boundary by the engagement ceiling. That last row
//! closed the §4.2c gate; what remains unwritten is stated at the bottom.

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use magician::magician_v2::{
    engagements::{
        authorize_engagement_delegation, authorize_engagement_dispatch,
        install_global_engagement_store, AuthorityDenial, EngagementAuthorityRef, EngagementStore,
    },
    execution::agentic::{
        execute_agentically, ActionExecutors, AgenticContext, AgenticPauseState, EnvironmentState,
        ExecutionNativeResponse, ExecutionToolCall, ShellState,
    },
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    slot_graph::extraction::{LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService},
    work_context::{WorkAuthorityRef, WorkContextKind},
};

/// Far enough ahead that no test sees an expiry it did not ask for.
const HORIZON_MS: i64 = 4_000;
const NOW_MS: i64 = 1_000;

/// The whole binary shares one installed store, because the thing under test
/// *is* the process-wide install. Scenarios are isolated by engagement id and
/// scope instead, so a revocation in one row cannot reach another.
async fn shared_store() -> Arc<EngagementStore> {
    use tokio::sync::OnceCell;
    static STORE: OnceCell<Arc<EngagementStore>> = OnceCell::const_new();
    STORE
        .get_or_init(|| async {
            // Leaked deliberately: the store must outlive every test in the
            // binary, and a TempDir dropped at the end of the first test would
            // pull the roster out from under the rest.
            let temp = Box::leak(Box::new(
                tempfile::tempdir().expect("engagement matrix tempdir"),
            ));
            let store = Arc::new(
                EngagementStore::open(temp.path())
                    .await
                    .expect("engagement store opens"),
            );
            install_global_engagement_store(store.clone());
            store
        })
        .await
        .clone()
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// The §4.2c scenario: a worker whose static grant is strictly broader than
/// the engagement ceiling.
async fn engagement_with(
    principal: &str,
    workspace: &str,
    ceiling: &[&str],
    team: &[&str],
) -> EngagementAuthorityRef {
    let store = shared_store().await;
    let authority = store
        .create(
            principal,
            workspace,
            "program-opc",
            "counterparty-acme",
            names(ceiling),
            names(team),
            NOW_MS,
            NOW_MS + HORIZON_MS,
        )
        .await
        .expect("engagement created");
    EngagementAuthorityRef {
        engagement_id: authority.engagement_id,
        authority_revision: authority.authority_revision,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Rows 1, 2, 3 — the intersection is what executes, and a name projection
// never advertised still dies at dispatch.
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn row_1_2_3_only_the_intersection_dispatches_and_layer_2_stands_alone() {
    let carried = engagement_with(
        "alpha",
        "prod",
        &["research", "restricted_email"],
        &["researcher"],
    )
    .await;

    // Inside the ceiling: allowed.
    for tool in ["research", "restricted_email"] {
        authorize_engagement_dispatch(&carried, "alpha", "prod", tool, NOW_MS + 1)
            .await
            .unwrap_or_else(|denial| panic!("`{tool}` is inside the ceiling: {denial:?}"));
    }

    // The worker's broader static grant: denied. `raw_email`, `browser` and
    // `files` are the §4.2c scenario's exact over-grant.
    for tool in ["raw_email", "browser", "files"] {
        let denial = authorize_engagement_dispatch(&carried, "alpha", "prod", tool, NOW_MS + 1)
            .await
            .expect_err("a tool outside the ceiling must be denied");
        assert!(
            matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }),
            "`{tool}` denied for the wrong reason: {denial:?}"
        );
    }

    // Row 2, the independence property: a name policy projection could never
    // have advertised — it is in no ceiling and no catalog — is refused by the
    // live check alone. A model that fabricates a tool name gets nothing.
    let denial = authorize_engagement_dispatch(
        &carried,
        "alpha",
        "prod",
        "tool_that_was_never_projected",
        NOW_MS + 1,
    )
    .await
    .expect_err("a fabricated tool name must die at the dispatch boundary");
    assert!(matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }));
}

// ─────────────────────────────────────────────────────────────────────────
// Row 4 — delegation is bounded by team[].
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn row_4_delegation_is_intersected_with_team() {
    let carried = engagement_with("beta", "prod", &["research"], &["researcher", "drafter"]).await;

    for target in ["researcher", "drafter"] {
        authorize_engagement_delegation(&carried, "beta", "prod", target, NOW_MS + 1)
            .await
            .unwrap_or_else(|denial| panic!("`{target}` is in team[]: {denial:?}"));
    }

    let denial = authorize_engagement_delegation(&carried, "beta", "prod", "auditor", NOW_MS + 1)
        .await
        .expect_err("a target outside team[] must be denied");
    assert!(matches!(denial, AuthorityDenial::TargetOutsideTeam { .. }));
}

// ─────────────────────────────────────────────────────────────────────────
// Row 7 — revocation blocks the NEXT consequential dispatch. The row most
// likely to be got wrong, because it is the only one that invalidates a
// snapshot that was valid when taken.
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn row_7_revocation_mid_run_denies_the_next_dispatch() {
    let store = shared_store().await;
    let carried = engagement_with("gamma", "prod", &["research"], &["researcher"]).await;

    // Valid before revocation — this is the snapshot that was legitimately taken.
    authorize_engagement_dispatch(&carried, "gamma", "prod", "research", NOW_MS + 1)
        .await
        .expect("allowed while live");

    store
        .revoke(&carried.engagement_id, NOW_MS + 2)
        .await
        .expect("revoke succeeds");

    // The child is still alive and still carries the same ref. The very next
    // dispatch dies, before any side effect.
    let denial = authorize_engagement_dispatch(&carried, "gamma", "prod", "research", NOW_MS + 3)
        .await
        .expect_err("a revoked engagement must deny immediately");
    assert!(
        matches!(denial, AuthorityDenial::Revoked),
        "revocation denied for the wrong reason: {denial:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Row 7 (expiry half) — no owner act required.
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn row_7_expiry_denies_without_an_owner_acting() {
    let carried = engagement_with("delta", "prod", &["research"], &["researcher"]).await;
    let denial = authorize_engagement_dispatch(
        &carried,
        "delta",
        "prod",
        "research",
        NOW_MS + HORIZON_MS + 1,
    )
    .await
    .expect_err("past expires_at_ms the engagement is dead");
    assert!(matches!(denial, AuthorityDenial::Expired));
}

// ─────────────────────────────────────────────────────────────────────────
// Rows 9, 10 — narrowing bumps the revision (staleness signal) and never
// widens.
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rows_9_10_narrowing_signals_staleness_and_never_widens() {
    let store = shared_store().await;
    let carried = engagement_with(
        "epsilon",
        "prod",
        &["research", "restricted_email"],
        &["researcher", "drafter"],
    )
    .await;

    store
        .narrow(
            &carried.engagement_id,
            Some(names(&["research"])),
            Some(names(&["researcher"])),
        )
        .await
        .expect("narrow succeeds");

    // Row 9: the carried revision is now behind, and the grant says so rather
    // than silently serving the pre-narrow authority.
    let grant = authorize_engagement_dispatch(&carried, "epsilon", "prod", "research", NOW_MS + 3)
        .await
        .expect("still inside the narrowed ceiling");
    assert!(
        grant.revision_changed,
        "a stale carried revision must be signalled for re-resolution"
    );

    // Row 10: narrowing removed `restricted_email`; nothing re-widens it.
    let denial =
        authorize_engagement_dispatch(&carried, "epsilon", "prod", "restricted_email", NOW_MS + 3)
            .await
            .expect_err("a narrowed-away tool must be denied");
    assert!(matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }));

    // And the delegation boundary narrowed with it.
    let denial =
        authorize_engagement_delegation(&carried, "epsilon", "prod", "drafter", NOW_MS + 3)
            .await
            .expect_err("a narrowed-away teammate must be denied");
    assert!(matches!(denial, AuthorityDenial::TargetOutsideTeam { .. }));
}

/// A widening attempt through `narrow()` is not merely ignored — the stored
/// ceiling stays the intersection, which is what makes "never widens"
/// structural rather than a convention.
#[tokio::test]
async fn row_10_narrow_cannot_widen_even_when_asked_to() {
    let store = shared_store().await;
    let carried = engagement_with("zeta", "prod", &["research"], &["researcher"]).await;

    store
        .narrow(
            &carried.engagement_id,
            Some(names(&["research", "raw_email", "browser"])),
            Some(names(&["researcher", "auditor"])),
        )
        .await
        .expect("narrow accepts the request");

    for tool in ["raw_email", "browser"] {
        let denial = authorize_engagement_dispatch(&carried, "zeta", "prod", tool, NOW_MS + 3)
            .await
            .expect_err("narrow() must intersect, never union");
        assert!(matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }));
    }
    let denial = authorize_engagement_delegation(&carried, "zeta", "prod", "auditor", NOW_MS + 3)
        .await
        .expect_err("team[] must intersect too");
    assert!(matches!(denial, AuthorityDenial::TargetOutsideTeam { .. }));
}

// ─────────────────────────────────────────────────────────────────────────
// Rows 3, 10 across in-context nested delegation.
//
// Nesting must be exercised in-context, not by spawning children: a spawned
// grandchild is force-failed today for an unrelated reason
// (`runtime.rs` "Delegated child yielded to nested children (not
// supported)"), so it would prove nothing about authority. The in-context
// path clones the parent context (`sub_ctx = ctx.clone()`), which is why the
// carriage assertion below is on the clone.
// ─────────────────────────────────────────────────────────────────────────

fn engagement_context(
    principal: &str,
    workspace: &str,
    carried: &EngagementAuthorityRef,
) -> AgenticContext {
    let mut ctx = AgenticContext::new("nested goal", "nested criteria");
    ctx.principal = Some(principal.to_string());
    ctx.workspace = Some(workspace.to_string());
    ctx.work_authority = Some(WorkAuthorityRef::from(carried));
    ctx
}

#[tokio::test]
async fn rows_3_10_an_in_context_nested_child_inherits_the_ceiling_and_cannot_widen() {
    let store = shared_store().await;
    let carried = engagement_with("eta", "prod", &["research"], &["researcher"]).await;
    let parent = engagement_context("eta", "prod", &carried);

    // The in-context delegation seam is a context clone. If the ref did not
    // survive it, a nested child would run unbounded — the whole point of
    // row 3.
    let child = parent.clone();
    let child_ref = child
        .engagement_ceiling_authority()
        .expect("an in-context nested child inherits the parent's engagement ref");
    let child_ref = &child_ref;
    assert_eq!(
        child_ref, &carried,
        "the nested child must carry the parent ref verbatim, not a variant of it"
    );

    // Row 3: raw outward tools stay unreachable through the nested child,
    // because the child resolves the same live authority as its parent.
    for tool in ["raw_email", "browser", "files"] {
        let denial = authorize_engagement_dispatch(child_ref, "eta", "prod", tool, NOW_MS + 1)
            .await
            .expect_err("a raw outward tool must be unreachable from a nested child");
        assert!(matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }));
    }

    // Row 10: narrowing the engagement narrows the nested child with it —
    // the child holds a reference, not a copy of the grant, so it cannot
    // outlive or exceed the parent's ceiling.
    store
        .narrow(&carried.engagement_id, Some(BTreeSet::new()), None)
        .await
        .expect("narrow to nothing succeeds");
    let denial = authorize_engagement_dispatch(child_ref, "eta", "prod", "research", NOW_MS + 2)
        .await
        .expect_err("an emptied ceiling must deny the nested child too");
    assert!(matches!(denial, AuthorityDenial::ToolOutsideCeiling { .. }));
}

/// The other half of row 5, at the nesting seam: a child context cannot name
/// a different engagement than the one it inherited. There is no path that
/// takes an engagement id from model-supplied data — the field is set from
/// the parent context and the persisted record only — so the strongest
/// available assertion is that an independently-constructed ref does not
/// resolve to the parent's authority.
#[tokio::test]
async fn row_5_a_child_cannot_name_an_engagement_it_was_not_given() {
    let carried = engagement_with("theta", "prod", &["research"], &["researcher"]).await;
    let parent = engagement_context("theta", "prod", &carried);

    // A child that invents its own id gets nothing: the store has no such
    // engagement, and an unknown id denies rather than defaulting open.
    let self_granted = EngagementAuthorityRef {
        engagement_id: "eng-self-granted".to_string(),
        authority_revision: 1,
    };
    let denial =
        authorize_engagement_dispatch(&self_granted, "theta", "prod", "research", NOW_MS + 1)
            .await
            .expect_err("a self-named engagement must not resolve");
    assert!(matches!(denial, AuthorityDenial::UnknownEngagement));

    // The inherited ref still works, so the denial above is about identity,
    // not about the scope being broken.
    authorize_engagement_dispatch(
        &parent
            .engagement_ceiling_authority()
            .expect("parent carries its ref"),
        "theta",
        "prod",
        "research",
        NOW_MS + 1,
    )
    .await
    .expect("the inherited engagement still authorizes");
}

// ─────────────────────────────────────────────────────────────────────────
// Row 11 — a non-engagement execution is unaffected. Proven by the shape of
// the carrier: the check runs only when a ref is present, so an execution
// without one reaches no engagement decision at all.
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn row_11_a_pause_state_without_an_engagement_carries_none() {
    let pause = base_pause_state();
    assert!(
        pause.work_authority.is_none(),
        "an ordinary execution must not acquire an engagement by default"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Rows 5, 6 — the carrier survives the transports that could drop it, and is
// bound to the pause so a tampered blob cannot swap it.
// ─────────────────────────────────────────────────────────────────────────

fn base_pause_state() -> AgenticPauseState {
    AgenticPauseState::new(
        2,
        "Draft the counterparty reply".to_string(),
        "Reply drafted and queued".to_string(),
        EnvironmentState::Shell(ShellState {
            working_dir: std::path::PathBuf::from("/tmp/engagement-matrix"),
            last_command: None,
            last_stdout: None,
            last_stderr: None,
            last_exit_code: None,
        }),
        "Iteration 1: read the brief".to_string(),
        10,
        3,
    )
    .with_observability("exec-engagement", "plan-engagement", "step-engagement")
}

/// Row 6, restart half: the durable pause is JSON, and the ref must come back
/// out of it byte-identical. If serialization dropped the field, a restarted
/// execution would silently resume unbounded.
#[tokio::test]
async fn row_6_the_carrier_survives_a_durable_restart_round_trip() {
    let mut pause = base_pause_state();
    pause.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: "eng-restart".to_string(),
        authority_revision: 7,
    }));

    let encoded = serde_json::to_string(&pause).expect("pause serializes");
    let restored: AgenticPauseState = serde_json::from_str(&encoded).expect("pause deserializes");

    let carried = restored
        .work_authority
        .as_ref()
        .expect("the work ref survived the restart");
    assert_eq!(
        carried.work,
        WorkContextKind::Engagement("eng-restart".to_string())
    );
    assert_eq!(carried.authority_revision, 7);
}

/// Rows 5 + 6, the binding half: the ref is folded into `authorization_hash`,
/// so a pause blob edited to name a different engagement no longer matches
/// its own authorization. Without this, a tampered pause is a self-service
/// engagement grant — the spoof row 5 exists to forbid.
#[tokio::test]
async fn row_5_6_a_tampered_pause_cannot_swap_the_engagement() {
    let mut pause = base_pause_state();
    pause.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: "eng-original".to_string(),
        authority_revision: 1,
    }));
    let sealed = pause.authorization_hash();

    // Same pause, different engagement: the hash must move.
    let mut tampered = pause.clone();
    tampered.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: "eng-attacker".to_string(),
        authority_revision: 1,
    }));
    assert_ne!(
        sealed,
        tampered.authorization_hash(),
        "swapping the engagement id must break the authorization hash"
    );

    // Same engagement, revision bumped by an attacker to dodge staleness:
    // also caught.
    let mut revision_forged = pause.clone();
    revision_forged.work_authority = Some(WorkAuthorityRef::from(&EngagementAuthorityRef {
        engagement_id: "eng-original".to_string(),
        authority_revision: 99,
    }));
    assert_ne!(
        sealed,
        revision_forged.authorization_hash(),
        "forging the authority revision must break the authorization hash"
    );

    // Dropping the engagement entirely — the cheapest attack, since an absent
    // ref means no check runs at all — must not pass as the same authorization.
    let mut stripped = pause.clone();
    stripped.work_authority = None;
    assert_ne!(
        sealed,
        stripped.authorization_hash(),
        "removing the engagement must break the authorization hash"
    );

    // An untouched round trip keeps its seal, or the binding would be useless
    // in normal operation.
    let encoded = serde_json::to_string(&pause).expect("pause serializes");
    let restored: AgenticPauseState = serde_json::from_str(&encoded).expect("pause deserializes");
    assert_eq!(
        sealed,
        restored.authorization_hash(),
        "an honest round trip must preserve the authorization hash"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Row 1a — the wiring itself, driven by a REAL execution.
//
// Every row above proves a *decision* against the live store through the same
// global wrappers `execute_action_inner` calls. None of them starts a run, so
// the wire between wrapper and executor stayed read-verified only. This row
// closes that: it drives `execute_agentically` — a public entry point,
// so no production surface moves for the test — and asserts the run is stopped
// at the dispatch boundary by engagement authority.
//
// Two deliberate choices, both stronger than the obvious alternative:
//
//   * The model stand-in returns the decision parser's own candidate JSON
//     rather than a serialized `Decision`. `Decision` is hand-parsed, and the
//     earlier note here proposed deriving `Deserialize` on it to make this
//     easier — that would have proven the executor authorizes a `Decision` the
//     real parser never produced. Emitting raw JSON runs the real parser.
//     The shape is the one already proven in-tree at `executor.rs:40726`.
//   * The prompt store is the real `data/magician_v2/prompts`, not a stub, so
//     the decision prompt renders exactly as it does in production.
// ─────────────────────────────────────────────────────────────────────────

/// `ActionExecutors::new` requires an `LlmService`, but the modern decide loop
/// does **not** consult it — decisions arrive as native tool calls through the
/// adapter below. This exists to satisfy the constructor and asserts as much:
/// if the legacy seam is ever consulted again, the run fails loudly rather than
/// silently taking a path this row does not model.
struct UnusedLlm;

#[async_trait]
impl LlmService for UnusedLlm {
    async fn call_function(
        &self,
        _request: LlmFunctionCallRequest,
    ) -> anyhow::Result<LlmFunctionCallResponse> {
        anyhow::bail!(
            "the legacy LlmService decision seam was consulted; this row scripts the \
             native tool-call adapter instead"
        )
    }
}

/// Decisions are scripted as **native tool calls**, mirroring
/// `native_response_from_decision_fixture` in-crate: the decision kind selects
/// the tool name, an `execute`/`tool` decision uses its `capability_name` as
/// that name, and the remaining keys become the call arguments.
fn scripted_executors(decisions: Vec<&str>) -> ActionExecutors {
    let storage_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root above the magician crate")
        .join("data")
        .join("magician_v2")
        .join("prompts");
    let storage = JsonPromptStorage::new(JsonStorageConfig {
        storage_dir,
        enable_cache: true,
        max_cache_entries: 128,
    })
    .expect("real prompt storage opens");

    let native_responses = decisions
        .iter()
        .map(|raw| {
            let value: serde_json::Value =
                serde_json::from_str(raw).expect("scripted decision must be valid JSON");
            let mut arguments = value
                .as_object()
                .expect("scripted decision must be an object")
                .clone();
            let decision = arguments
                .remove("decision")
                .and_then(|value| value.as_str().map(str::to_owned))
                .expect("scripted decision kind");
            let tool_name = match decision.as_str() {
                "execute" => {
                    let action_type = arguments
                        .remove("action_type")
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .expect("scripted execute action type");
                    if action_type == "tool" {
                        arguments
                            .remove("capability_name")
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .expect("scripted tool capability")
                    } else {
                        action_type
                    }
                },
                other => other.to_owned(),
            };
            arguments.insert(
                "task_state_action".to_owned(),
                serde_json::json!({
                    "action": "none",
                    "reason": "No durable task-state change is needed for this test."
                }),
            );
            let mut response = ExecutionNativeResponse::from_tool_calls(vec![ExecutionToolCall {
                id: "engagement-matrix-tool-call".to_owned(),
                name: tool_name,
                arguments: serde_json::Value::Object(arguments),
            }]);
            response.finish_reason = Some("tool_calls".to_owned());
            response
        })
        .collect();

    ActionExecutors::new(
        Arc::new(UnusedLlm),
        Arc::new(PromptManager::new(Arc::new(storage))),
    )
    .with_native_adapter(Arc::new(
        magician::magician_v2::execution::MultiLlmAgentAdapter::new_with_test_native_responses(
            native_responses,
        ),
    ))
}

/// A real, registered capability that the engagement does not admit.
///
/// `execute` panics on purpose. The row's whole claim is that authority stops
/// the call at the *dispatch boundary* — before the provider runs — so a
/// provider that runs is a failed assertion, not an incidental side effect.
#[derive(Debug)]
struct EngagementProbeProvider {
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl magician::magician_v2::execution::capability::CapabilityProvider for EngagementProbeProvider {
    fn tool_name(&self) -> &str {
        "engagement_probe"
    }

    fn lower(
        &self,
        step: &magician::magician_v2::strategy::plan::PlanStep,
    ) -> std::result::Result<
        magician::magician_v2::resource_authority::gated_action::MaybeGatedAction,
        magician::magician_v2::execution::error::ExecutionError,
    > {
        Ok(
            magician::magician_v2::resource_authority::gated_action::MaybeGatedAction::Bare(
                magician::magician_v2::execution::actions::ExecutableAction::Pack {
                    capability_name: "engagement_probe".to_string(),
                    implementation:
                        magician::magician_v2::execution::capability::ImplementationType::Compiled {
                            provider_name: "engagement_probe".to_string(),
                        },
                    resolved_params: step.parameters.clone(),
                },
            ),
        )
    }

    async fn execute(
        &self,
        _action: &magician::magician_v2::execution::actions::ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> std::result::Result<
        magician::magician_v2::execution::actions::ActionResult,
        magician::magician_v2::execution::error::ExecutionError,
    > {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
            magician::magician_v2::execution::actions::ActionResult::text(
                "engagement probe executed",
            ),
        )
    }
}

/// Drive one live run whose scripted model calls `engagement_probe`, under an
/// engagement with the given ceiling. Returns how many times the provider
/// actually executed.
///
/// The terminal `AgenticOutcome` carries no action history, so asserting on the
/// denial *text* is not possible from here. Provider execution is the honest
/// observable: it is the thing the ceiling either permits or prevents.
async fn probe_executions_under_ceiling(
    principal: &str,
    workspace: &str,
    ceiling: &[&str],
) -> (usize, String) {
    // Executor internals are otherwise invisible from an integration binary,
    // and a silent zero is exactly the failure this row must be able to explain.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("magician=debug")),
        )
        .with_test_writer()
        .try_init();

    // A live run stamps its own wall clock, so this engagement must be valid
    // NOW — not at the synthetic `NOW_MS` the wrapper-level rows pass in by
    // hand. With the 1970-era constants both controls were denied for expiry,
    // which looks exactly like a working ceiling denial from the outside.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_millis() as i64;
    let created = shared_store()
        .await
        .create(
            principal,
            workspace,
            "program-opc",
            "counterparty-acme",
            names(ceiling),
            BTreeSet::new(),
            now_ms - 60_000,
            now_ms + 3_600_000,
        )
        .await
        .expect("live engagement created");
    let authority = EngagementAuthorityRef {
        engagement_id: created.engagement_id,
        authority_revision: created.authority_revision,
    };

    let mut executors = scripted_executors(vec![
        r#"{"decision":"execute","action_type":"tool","capability_name":"engagement_probe"}"#,
        r#"{"decision":"goal_reached","evidence":"Probe attempt settled.","artifacts":[]}"#,
    ]);

    let pack: magician::magician_v2::execution::capability::CapabilityPackDefinition =
        serde_yaml::from_str(
            r#"
name: engagement_probe
description: Registered probe used to prove engagement denial at the dispatch boundary.
version: "1.0.0"
parameters: []
implementation:
  type: compiled
  provider_name: engagement_probe
execution:
  requires_browser_session: false
  default_timeout_secs: 5
  categories: [test]
  composition_category: action
  sandbox: none
"#,
        )
        .expect("engagement probe pack");

    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry =
        Arc::new(magician::magician_v2::execution::capability::CapabilityRegistry::new());
    registry.register(Arc::new(EngagementProbeProvider {
        executions: Arc::clone(&executions),
    }));
    registry.set_pack_definition("engagement_probe", pack.clone());
    executors.capability_registry = Some(Arc::clone(&registry));

    // Built from JSON rather than a struct literal so the row does not pin
    // `ParameterDefinition`'s shape; the serde defaults cover the rest.
    let tool: runtime_core::ToolInfo = serde_json::from_value(serde_json::json!({
        "name": "engagement_probe",
        "description": "Registered probe used to prove engagement denial at dispatch.",
        "category": "test",
        "categories": ["test"],
        "parameters": [],
        "enhanced_description": null,
        "keywords": [],
        "use_cases": []
    }))
    .expect("probe tool info");

    // The context path, not the pause path. `execute_agentically_continue`
    // rebuilds its context via `restore_context_from_pause`, which does not
    // populate `tool_index` / `loaded_tools` / `merged_agent_tools` — so the
    // probe was never dispatchable there and BOTH controls read zero, which is
    // indistinguishable from a working denial. Driving a context we own is what
    // makes the admitted control able to reach the provider at all.
    let mut ctx = AgenticContext::new(
        "Attempt one engagement-scoped probe",
        "The probe is either dispatched or refused by engagement authority",
    );
    ctx.principal = Some(principal.to_string());
    ctx.workspace = Some(workspace.to_string());
    ctx.work_authority = Some(WorkAuthorityRef::from(&authority));
    ctx.tool_index = Some(Arc::new(
        magician::magician_v2::execution::flat_loop::build_tool_index(&[pack]),
    ));
    ctx.scratch
        .loaded_tools
        .lock()
        .expect("loaded tools")
        .insert("engagement_probe".to_string());
    ctx.merged_agent_tools = vec![tool];

    let initial_state = EnvironmentState::Shell(ShellState {
        working_dir: std::path::PathBuf::from("/tmp/engagement-matrix"),
        last_command: None,
        last_stdout: None,
        last_stderr: None,
        last_exit_code: None,
    });

    let outcome = execute_agentically(&ctx, initial_state, &executors, None)
        .await
        .expect("the run reaches a terminal outcome rather than erroring out of the harness");

    (
        executions.load(std::sync::atomic::Ordering::SeqCst),
        format!("{outcome:?}"),
    )
}

/// Row 1a — the wire, driven by a real execution, as a controlled pair.
///
/// A single negative case would not prove the *ceiling* stopped the call: a
/// misconfigured harness that never dispatches at all produces the same zero.
/// So the row runs the identical scenario twice and varies exactly one thing —
/// whether the engagement's ceiling contains the probe. If the excluded run
/// executes nothing and the admitted run executes once, the ceiling is the
/// deciding variable and the wrapper is genuinely on the executor's path.
#[tokio::test]
async fn row_1a_a_live_execution_is_denied_at_the_dispatch_boundary() {
    let (denied, denied_outcome) =
        probe_executions_under_ceiling("principal-denied", "workspace-denied", &["presto-gmail"])
            .await;
    assert_eq!(
        denied, 0,
        "a capability outside the engagement ceiling reached the provider — the \
         dispatch boundary did not stop it. Outcome: {denied_outcome}"
    );

    let (admitted, admitted_outcome) = probe_executions_under_ceiling(
        "principal-admitted",
        "workspace-admitted",
        &["engagement_probe"],
    )
    .await;
    assert_eq!(
        admitted, 1,
        "a capability INSIDE the ceiling never reached the provider, so the zero \
         above proves nothing about the ceiling — the harness is not dispatching. \
         Outcome: {admitted_outcome}"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// What this file does NOT prove — stated here so a passing run is not read as
// covering more than it does:
//
// * Row 12: **the row cannot be written as specified.** "Capture mode" appears
//   exactly twice in the parent design, both times inside row 12 itself, and
//   no execution flag in this codebase corresponds to it — the only "capture"
//   here is captured *auth* for API replay, a different concept. Writing a
//   test against a guess would prove nothing, so the row waits on the design
//   owner naming what a capture-mode execution is.
//
// Rows 3 and 10 across in-context nested delegation ARE covered above.
//
// A note for whoever adds the next executor-driven row, because each of these
// cost a build cycle and all three LOOK like a working denial from outside:
//
//   1. Decisions do NOT ride `LlmService`. They are native tool calls through
//      `MultiLlmAgentAdapter::new_with_test_native_responses`. A mock on the
//      `LlmService` seam is never consulted and burns the 5-strike parse
//      counter instead.
//   2. The context path is required. `execute_agentically_continue` rebuilds
//      its context via `restore_context_from_pause`, which does not populate
//      `tool_index` / `loaded_tools` / `merged_agent_tools`, so nothing is
//      dispatchable and every control reads zero.
//   3. A live run stamps its own wall clock. Engagements built on this file's
//      synthetic `NOW_MS` are decades expired by then, and expiry denial is
//      indistinguishable from ceiling denial unless you assert the admitted
//      case too.
//
// That is why row 1a is a controlled PAIR. A lone negative case passes
// trivially whenever the harness dispatches nothing at all.
// ─────────────────────────────────────────────────────────────────────────
