//! Opt-in, bounded drift verification for monitor-owned Task Recipes.
//!
//! Verification never creates a browser session, heals auth, requests write
//! approval, or falls through to an agent. Only Trusted, read-only recipes
//! whose current version was learned from a typed recurring Monitor are
//! eligible. The feature remains default-off.

use super::origin_policy::{OriginPolicyStore, OriginReplayMode};
use super::recipe::{RecipeMaturity, TaskRecipe};
use super::recipe_packs::publish_recipe_pack;
use super::recipe_runner::{
    RecipeRunInputs, RecipeRunner, ReqwestTransport, DEFAULT_RECIPE_TIMEOUT_MS,
};
use super::recipe_runs::{RecipeRunLedger, RecipeRunRecord};
use super::recipe_store::RecipeStore;
use super::replay_grants::ReplayGrantStore;
use super::switch::{runtime_api_mining_config, ApiMiningSwitch};
use crate::config::ApiMiningConfig;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::{CapabilityPackStore, ScopedCapabilityResolver};
use crate::magician_v2::secrets::SecretStoreResolver;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_RECIPES_PER_CYCLE: usize = 32;

/// Start the process-lived worker only when boot configuration opts in. The
/// first request is delayed by the full interval, so startup never creates
/// surprise background traffic.
pub fn spawn_if_enabled(
    workspace_layout: ArtifactV2Workspace,
    secret_store_resolver: Arc<SecretStoreResolver>,
    api_mining_switch: ApiMiningSwitch,
    capability_resolver: Arc<ScopedCapabilityResolver>,
    config: ApiMiningConfig,
) -> Option<tokio::task::JoinHandle<()>> {
    let config = config.validated();
    if !verification_enabled(&config) {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut interval_secs = config.recipe_verification.interval_secs;
        loop {
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            let Some(runtime_config) = runtime_api_mining_config() else {
                continue;
            };
            interval_secs = runtime_config.recipe_verification.interval_secs.max(1);
            if !verification_enabled(&runtime_config) {
                continue;
            }
            run_cycle(
                &workspace_layout,
                &secret_store_resolver,
                &api_mining_switch,
                &capability_resolver,
                MAX_RECIPES_PER_CYCLE,
            )
            .await;
        }
    }))
}

fn verification_enabled(config: &ApiMiningConfig) -> bool {
    config.enabled
        && config.recipes.enabled
        && config.recipe_verification.enabled
        && config
            .recipes
            .transport_ladder
            .iter()
            .any(|transport| transport.eq_ignore_ascii_case("reqwest"))
}

fn runtime_verification_enabled() -> bool {
    runtime_api_mining_config()
        .as_ref()
        .is_some_and(verification_enabled)
}

fn verification_candidate(recipe: &TaskRecipe) -> bool {
    let Some(version) = recipe.current() else {
        return false;
    };
    version.maturity == RecipeMaturity::Trusted
        && version.compiled_from.monitor_revision.is_some()
        && !version.answer_spec.is_empty()
        && recipe.is_read_only()
}

fn verification_inputs(recipe: &TaskRecipe) -> HashMap<String, String> {
    recipe
        .shape
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.example_value.clone()))
        .collect()
}

async fn run_cycle(
    workspace_layout: &ArtifactV2Workspace,
    secret_store_resolver: &SecretStoreResolver,
    api_mining_switch: &ApiMiningSwitch,
    capability_resolver: &ScopedCapabilityResolver,
    budget: usize,
) {
    let mut remaining = budget;
    let mut scopes = workspace_layout.list_tenant_scopes();
    scopes.sort_unstable();
    for (principal, workspace) in scopes {
        if remaining == 0 {
            break;
        }
        if !runtime_verification_enabled() {
            return;
        }
        if !api_mining_switch.effective(&principal, &workspace) {
            continue;
        }
        let mining_base = workspace_layout.api_mining_root(&principal, &workspace);
        let store = RecipeStore::new(mining_base.clone());
        let mut recipes = match store.list() {
            Ok(recipes) => recipes,
            Err(error) => {
                tracing::warn!(
                    %principal,
                    %workspace,
                    %error,
                    "[API_MINING] recipe verification could not list recipes"
                );
                continue;
            },
        };
        recipes.retain(|recipe| verification_candidate(recipe));
        // Oldest verification first gives a bounded cycle fair rotation when
        // a scope contains more recipes than the process budget.
        recipes.sort_by(|left, right| {
            left.current()
                .and_then(|version| version.last_replayed_at_ms)
                .cmp(
                    &right
                        .current()
                        .and_then(|version| version.last_replayed_at_ms),
                )
                .then_with(|| left.id.cmp(&right.id))
        });
        if recipes.is_empty() {
            continue;
        }
        let secret_store = match secret_store_resolver.resolve_for_scope(&principal, &workspace) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(
                    %principal,
                    %workspace,
                    %error,
                    "[API_MINING] recipe verification could not resolve captured auth"
                );
                continue;
            },
        };

        for candidate in recipes {
            if remaining == 0 {
                break;
            }
            let replay_lock = match store.replay_lock(&candidate.id) {
                Ok(lock) => lock,
                Err(error) => {
                    tracing::warn!(
                        recipe_id = %candidate.id,
                        %error,
                        "[API_MINING] recipe verification rejected an invalid recipe id"
                    );
                    continue;
                },
            };
            let _replay_guard = replay_lock.lock().await;
            if !api_mining_switch.effective(&principal, &workspace)
                || !runtime_verification_enabled()
            {
                break;
            }
            let mut recipe = match store.load(&candidate.id) {
                Ok(Some(recipe)) if verification_candidate(&recipe) => recipe,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(
                        recipe_id = %candidate.id,
                        %error,
                        "[API_MINING] recipe verification could not reload recipe"
                    );
                    continue;
                },
            };
            if recipe.scope_principal != principal || recipe.scope_workspace != workspace {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    %principal,
                    %workspace,
                    "[API_MINING] recipe verification rejected mismatched durable scope"
                );
                continue;
            }
            let policy = OriginPolicyStore::open(&mining_base);
            let origin_allowed = recipe.current().is_some_and(|version| {
                version.steps.iter().all(|step| {
                    !policy.is_blocked(&step.origin)
                        && !matches!(
                            policy.live_replay_mode_for_origin(&step.origin),
                            OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
                        )
                })
            });
            if !origin_allowed {
                continue;
            }

            remaining = remaining.saturating_sub(1);
            let grants = ReplayGrantStore::open(&mining_base);
            let session_lookup = |origin: &str, url: &str| {
                secret_store
                    .get_session(origin, url)
                    .map(|(session, _lease)| session)
            };
            let can_continue = || {
                runtime_verification_enabled()
                    && api_mining_switch.effective(&principal, &workspace)
            };
            let runner = RecipeRunner {
                can_continue: Some(&can_continue),
                transports: vec![Box::new(ReqwestTransport::default())],
                grants: &grants,
                origin_policy: &policy,
                session_lookup: &session_lookup,
                auth_healer: None,
                max_auth_heals: 0,
                step_feedback: None,
                observer: None,
            };
            let started = Instant::now();
            let inputs = verification_inputs(&recipe);
            let metrics = super::recipe_metrics_for_scope(&principal, &workspace);
            metrics.record_replay_started();
            let result = runner
                .run(
                    &mut recipe,
                    &RecipeRunInputs {
                        inputs,
                        timeout_ms: Some(DEFAULT_RECIPE_TIMEOUT_MS),
                        approved_write_steps: Default::default(),
                    },
                )
                .await;
            if !api_mining_switch.effective(&principal, &workspace)
                || !runtime_verification_enabled()
            {
                continue;
            }
            // A policy block committed during the request is a hard commit
            // fence. The matching purge holds this recipe lock after us and
            // removes the old record; do not briefly republish its pack.
            if recipe.current().is_some_and(|version| {
                version
                    .steps
                    .iter()
                    .any(|step| policy.is_blocked(&step.origin))
            }) {
                continue;
            }
            if result.success {
                metrics.record_replay_succeeded();
            } else {
                metrics.record_replay_failed(
                    result
                        .failure
                        .as_ref()
                        .map(|failure| failure.class)
                        .or_else(|| result.fallback.as_ref().map(|fallback| fallback.class)),
                );
            }
            if let Err(error) = store.save(&recipe) {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    %error,
                    "[API_MINING] recipe verification could not save replay health"
                );
                continue;
            }
            let execution_id = format!("verify_{}", ulid::Ulid::new());
            let record = RecipeRunRecord::from_result(
                &recipe,
                &result,
                recipe
                    .current()
                    .map(|version| version.compiled_from.task_id.clone())
                    .unwrap_or_else(|| format!("recipe_{}", recipe.id)),
                execution_id,
                "verification",
                started.elapsed().as_millis() as u64,
                Vec::new(),
            );
            if let Err(error) = RecipeRunLedger::new(&mining_base).append(&record).await {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    %error,
                    "[API_MINING] recipe verification ledger append failed"
                );
            }
            let pack_store = CapabilityPackStore::with_workspace_layout(
                workspace_layout,
                &principal,
                &workspace,
            );
            match publish_recipe_pack(
                &pack_store,
                &workspace_layout.scope_skills_root(&principal, &workspace),
                &recipe,
            ) {
                Ok(true) => {
                    capability_resolver.invalidate_scope(&principal, &workspace);
                },
                Ok(false) => {},
                Err(error) => tracing::warn!(
                    recipe_id = %recipe.id,
                    %error,
                    "[API_MINING] recipe verification pack refresh failed"
                ),
            }
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::SideEffects;
    use crate::magician_v2::api_mining::recipe::{
        AnswerField, CompiledFrom, Extractor, RecipeAuth, RecipeShape, RecipeStep, RecipeVersion,
    };
    use crate::magician_v2::api_mining::workflow::ReplayStats;

    fn recipe(
        maturity: RecipeMaturity,
        monitor_revision: Option<u32>,
        side_effects: SideEffects,
    ) -> TaskRecipe {
        TaskRecipe {
            id: "rcp_verify".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "personal-assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "check source".into(),
                fingerprint: "shape".into(),
                inputs: Vec::new(),
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec!["https://example.test".into()],
                steps: vec![RecipeStep {
                    id: "step_1".into(),
                    origin: "https://example.test".into(),
                    method: "GET".into(),
                    url_template: "https://example.test/status".into(),
                    headers_template: HashMap::new(),
                    body_template: None,
                    capability_id: None,
                    param_sources: HashMap::new(),
                    body_param_types: HashMap::new(),
                    side_effects,
                    request_shape_fingerprint: "request-shape".into(),
                    verify_with: None,
                    browser_fallback: None,
                    transport_hint: None,
                }],
                data_flows: Vec::new(),
                answer_spec: vec![AnswerField {
                    field: "status".into(),
                    step_id: "step_1".into(),
                    extractor: Extractor::JsonPath {
                        path: "$.status".into(),
                    },
                }],
                auth: RecipeAuth::default(),
                maturity,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "monitor_task".into(),
                    execution_id: "monitor_execution".into(),
                    task_text_fingerprint: None,
                    monitor_revision,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn only_trusted_monitor_owned_read_recipes_are_verified() {
        assert!(verification_candidate(&recipe(
            RecipeMaturity::Trusted,
            Some(1),
            SideEffects::ReadOnly,
        )));
        assert!(!verification_candidate(&recipe(
            RecipeMaturity::Validated,
            Some(1),
            SideEffects::ReadOnly,
        )));
        assert!(!verification_candidate(&recipe(
            RecipeMaturity::Trusted,
            None,
            SideEffects::ReadOnly,
        )));
        assert!(!verification_candidate(&recipe(
            RecipeMaturity::Trusted,
            Some(1),
            SideEffects::Write,
        )));
    }

    #[test]
    fn verification_requires_every_runtime_gate_and_reqwest() {
        let mut config = ApiMiningConfig::default();
        assert!(!verification_enabled(&config));

        config.recipe_verification.enabled = true;
        assert!(verification_enabled(&config));

        config.recipes.transport_ladder = vec!["browser".into()];
        assert!(!verification_enabled(&config));

        config.recipes.transport_ladder = vec!["REQWEST".into()];
        config.recipes.enabled = false;
        assert!(!verification_enabled(&config));

        config.recipes.enabled = true;
        config.enabled = false;
        assert!(!verification_enabled(&config));
    }
}
