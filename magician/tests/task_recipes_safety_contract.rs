//! Cross-cutting safety contracts for Task Recipes.
//!
//! Network sends are counted by the transport itself, so a policy regression
//! cannot be hidden by a convenient result enum. These contracts deliberately
//! avoid real network access and provider calls.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use magician::magician_v2::{
    api_mining::{
        approval::{options_for, OPTION_APPROVE_ALWAYS_STEP},
        capability::SideEffects,
        origin_policy::{OriginPolicyStore, OriginReplayMode},
        recipe::{
            request_shape_fingerprint, AnswerField, CompiledFrom, RecipeAuth, RecipeMaturity,
            RecipeShape, RecipeStep, RecipeVersion, TaskInput, TaskInputSchema, TaskInputSource,
            TaskRecipe, Transport,
        },
        recipe_compiler::{
            compile_task_recipe, llm_fallback::serialize_shape, values::collect_reported_values,
            RecipeCompileInput,
        },
        recipe_matcher::{
            eligible_for_fuzzy, serialize_match_payload, RecipeMatcher, TaskShapeQuery,
        },
        recipe_runner::{
            AuthHealer, FailureClass, RecipeRunInputs, RecipeRunner, StepTransport,
            TransportRequest, TransportResponse,
        },
        recipe_store::RecipeStore,
        replay_grants::{is_denylisted_url_template, GrantKey, ReplayGrantStore},
        switch::ApiMiningSwitch,
        types::{NetworkTraceEvent, SessionContext},
        workflow::ReplayStats,
    },
    artifact_v2::recipe_replay_hook::RecipeReplayAttempt,
};

#[derive(Clone)]
struct CountingTransport {
    sends: Arc<AtomicUsize>,
    response: Result<TransportResponse, String>,
}

#[async_trait]
impl StepTransport for CountingTransport {
    fn kind(&self) -> Transport {
        Transport::Reqwest
    }

    async fn send(&self, _request: &TransportRequest) -> Result<TransportResponse, String> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.response.clone()
    }
}

struct CountingHealer(AtomicUsize);

#[async_trait]
impl AuthHealer for CountingHealer {
    async fn heal(&self, _origin: &str) -> Result<bool, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

fn response(status: u16, body: &str) -> TransportResponse {
    TransportResponse {
        status,
        headers: HashMap::new(),
        body: body.into(),
    }
}

fn recipe(side_effects: SideEffects, url: &str) -> TaskRecipe {
    let is_write = side_effects == SideEffects::Write;
    let method = if is_write { "POST" } else { "GET" };
    let origin = url::Url::parse(url).unwrap().origin().ascii_serialization();
    let mut recipe = TaskRecipe {
        id: "rcp_contract".into(),
        scope_principal: "anonymous".into(),
        scope_workspace: "default".into(),
        agent_id: "personal-assistant".into(),
        shape: RecipeShape {
            description_template: None,
            template: "contract task".into(),
            fingerprint: "contract-shape".into(),
            inputs: is_write
                .then(|| TaskInput {
                    name: "value".into(),
                    schema: TaskInputSchema::String,
                    example_value: "expected-value".into(),
                    source: TaskInputSource::TaskText,
                })
                .into_iter()
                .collect(),
        },
        current_version: 1,
        versions: vec![RecipeVersion {
            version: 1,
            origins: vec![origin.clone()],
            steps: vec![RecipeStep {
                id: "s0".into(),
                origin,
                method: method.into(),
                url_template: url.into(),
                headers_template: HashMap::new(),
                body_template: is_write.then(|| r#"{"value":"{value}"}"#.to_string()),
                capability_id: None,
                param_sources: if is_write {
                    HashMap::from([(
                        "value".into(),
                        magician::magician_v2::api_mining::recipe::RecipeParamSource::TaskInput {
                            name: "value".into(),
                        },
                    )])
                } else {
                    HashMap::new()
                },
                body_param_types: HashMap::new(),
                side_effects,
                request_shape_fingerprint: request_shape_fingerprint(
                    method,
                    url,
                    is_write.then_some(r#"{"value":"{value}"}"#),
                ),
                verify_with: None,
                browser_fallback: None,
                transport_hint: None,
            }],
            data_flows: Vec::new(),
            answer_spec: vec![AnswerField {
                field: "ok".into(),
                step_id: "s0".into(),
                extractor: magician::magician_v2::api_mining::recipe::Extractor::JsonPath {
                    path: "$.ok".into(),
                },
            }],
            auth: RecipeAuth::default(),
            maturity: RecipeMaturity::Draft,
            replay_stats: ReplayStats::default(),
            compiled_from: CompiledFrom {
                task_id: "task_contract".into(),
                execution_id: "exec_contract".into(),
                task_text_fingerprint: None,
                monitor_revision: None,
                sequence_ids: Vec::new(),
                trace_files: Vec::new(),
            },
            compiled_at_ms: 0,
            last_replayed_at_ms: None,
        }],
    };
    if is_write {
        let version = recipe.current_mut().unwrap();
        version.steps[0].verify_with = Some("s_verify".into());
        let mut verify = version.steps[0].clone();
        verify.id = "s_verify".into();
        verify.method = "GET".into();
        verify.side_effects = SideEffects::ReadOnly;
        verify.body_template = None;
        verify.param_sources.clear();
        verify.verify_with = None;
        version.steps.push(verify);
        version.answer_spec[0].step_id = "s_verify".into();
    }
    recipe
}

fn write_inputs(approved: bool) -> RecipeRunInputs {
    RecipeRunInputs {
        inputs: HashMap::from([("value".into(), "expected-value".into())]),
        approved_write_steps: approved.then(|| "s0".to_string()).into_iter().collect(),
        ..Default::default()
    }
}

async fn run(
    recipe: &mut TaskRecipe,
    transport: CountingTransport,
    grants: &ReplayGrantStore,
    policy: &OriginPolicyStore,
    inputs: RecipeRunInputs,
    healer: Option<&dyn AuthHealer>,
    max_auth_heals: u32,
) -> magician::magician_v2::api_mining::recipe_runner::RecipeRunResult {
    let lookup = |_: &str, _: &str| Some(SessionContext::default());
    RecipeRunner {
        can_continue: None,
        transports: vec![Box::new(transport)],
        grants,
        origin_policy: policy,
        session_lookup: &lookup,
        auth_healer: healer,
        max_auth_heals,
        step_feedback: None,
        observer: None,
    }
    .run(recipe, &inputs)
    .await
}

#[tokio::test]
async fn invariant_01_ungranted_write_is_never_sent() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let mut recipe = recipe(SideEffects::Write, "https://example.test/api/cart");
    let read_url = "https://example.test/api/cart/context";
    recipe.current_mut().unwrap().steps.insert(
        0,
        RecipeStep {
            id: "s_read_before_write".into(),
            origin: "https://example.test".into(),
            method: "GET".into(),
            url_template: read_url.into(),
            headers_template: HashMap::new(),
            body_template: None,
            capability_id: None,
            param_sources: HashMap::new(),
            body_param_types: HashMap::new(),
            side_effects: SideEffects::ReadOnly,
            request_shape_fingerprint: request_shape_fingerprint("GET", read_url, None),
            verify_with: None,
            browser_fallback: None,
            transport_hint: None,
        },
    );
    let result = run(
        &mut recipe,
        CountingTransport {
            sends: Arc::clone(&sends),
            response: Ok(response(200, "{\"ok\":true}")),
        },
        &grants,
        &policy,
        write_inputs(false),
        None,
        0,
    )
    .await;
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert!(result.pending_approval.is_some());
    assert!(result.fallback.is_none());
}

#[test]
fn invariant_02_denylisted_writes_cannot_receive_durable_grants() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let url = "https://example.test/api/checkout/confirm";
    let key = GrantKey {
        recipe_id: Some("rcp_contract".into()),
        step_id: Some("s0".into()),
        capability_id: None,
        request_shape_fingerprint: request_shape_fingerprint("POST", url, None),
    };
    assert!(is_denylisted_url_template(url));
    assert!(grants.grant_for_url(&key, url, None).is_err());
    assert!(!options_for(url)
        .iter()
        .any(|option| option.id == OPTION_APPROVE_ALWAYS_STEP));
}

#[tokio::test]
async fn invariant_02b_opaque_write_ignores_even_a_preexisting_grant() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let url = "https://example.test/api/cart";
    let mut recipe = recipe(SideEffects::Write, url);
    let recipe_id = recipe.id.clone();
    let (step_id, request_shape_fingerprint) = {
        let step = &mut recipe.current_mut().unwrap().steps[0];
        step.body_template = Some("{value}".into());
        (step.id.clone(), step.effective_request_shape_fingerprint())
    };
    let grant_key = GrantKey {
        recipe_id: Some(recipe_id),
        step_id: Some(step_id),
        capability_id: None,
        request_shape_fingerprint,
    };
    grants
        .grant_for_url(&grant_key, url, Some("legacy-grant"))
        .unwrap();

    let result = run(
        &mut recipe,
        CountingTransport {
            sends: Arc::clone(&sends),
            response: Ok(response(200, "{}")),
        },
        &grants,
        &policy,
        write_inputs(false),
        None,
        0,
    )
    .await;

    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert!(result
        .pending_approval
        .as_ref()
        .is_some_and(|pending| pending.always_ask));
}

#[tokio::test]
async fn invariant_03_unknown_side_effects_are_never_sent() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let mut recipe = recipe(SideEffects::Unknown, "https://example.test/api/action");
    let result = run(
        &mut recipe,
        CountingTransport {
            sends: Arc::clone(&sends),
            response: Ok(response(200, "{}")),
        },
        &grants,
        &policy,
        RecipeRunInputs::default(),
        None,
        0,
    )
    .await;
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert_eq!(result.fallback.unwrap().class, FailureClass::PolicyBlocked);
}

#[tokio::test]
async fn invariant_04_failed_write_is_sent_exactly_once_without_browser_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let mut recipe = recipe(SideEffects::Write, "https://example.test/api/cart");
    let result = run(
        &mut recipe,
        CountingTransport {
            sends: Arc::clone(&sends),
            response: Ok(response(503, "failed")),
        },
        &grants,
        &policy,
        write_inputs(true),
        None,
        0,
    )
    .await;
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert!(result.failure.is_some());
    assert!(result.fallback.is_none());
}

#[test]
fn invariant_05_denied_write_is_a_terminal_denial_not_fallback() {
    let attempt = RecipeReplayAttempt::WriteDenied {
        recipe_id: "rcp_contract".into(),
        step_id: "s0".into(),
    };
    assert!(matches!(&attempt, RecipeReplayAttempt::WriteDenied { .. }));
    assert!(!matches!(&attempt, RecipeReplayAttempt::Fallback { .. }));
}

#[test]
fn invariants_06_and_07_compiler_and_llm_payload_never_retain_session_material() {
    let trace: NetworkTraceEvent = serde_json::from_value(serde_json::json!({
        "request_id": "secret-trace",
        "method": "POST",
        "url": "https://example.test/login?apikey=query-secret-value",
        "resource_type": "Fetch",
        "request_headers": {
            "content-type": "application/json",
            "authorization": "Bearer header-secret-value",
			"x-api-key": "api-key-secret-value",
			"x-client-token": "client-token-secret-value",
            "cookie": "sid=cookie-secret-value"
        },
        "request_body": "{\"username\":\"fixture-user\",\"password\":\"password-secret-value\"}",
        "response_headers": {"content-type": "application/json"},
        "response_body": "{\"state\":\"authenticated\"}",
        "status": 200,
        "timing": {"request_time": 1.0, "dns_duration": null, "connect_duration": null, "ssl_duration": null, "ttfb": 1.0, "total_duration": 2.0},
        "initiator": {"initiator_type": "script", "stack": null, "url": "https://example.test/"},
        "timestamp": 1780000000000_i64,
        "request_size": 100,
        "response_size": 25
    }))
    .unwrap();
    let reported =
        collect_reported_values("", &[serde_json::json!({"state": "authenticated"})], &[]);
    let recipe = compile_task_recipe(&RecipeCompileInput {
        task_id: "task_secret".into(),
        execution_id: "exec_secret".into(),
        monitor_revision: None,
        agent_id: "personal-assistant".into(),
        principal: "anonymous".into(),
        workspace: "default".into(),
        task_title: "Log in fixture-user".into(),
        task_text: String::new(),
        reported,
        traces: vec![trace],
        typed_inputs: Vec::new(),
        trace_files: Vec::new(),
        sequences: Vec::new(),
    })
    .unwrap();
    let durable = serde_json::to_string(&recipe).unwrap();
    let llm = serialize_shape(&recipe, "Log in fixture-user");
    let matcher = serialize_match_payload("Open account page", "", &[&recipe]);
    for secret in [
        "query-secret-value",
        "header-secret-value",
        "api-key-secret-value",
        "client-token-secret-value",
        "cookie-secret-value",
        "password-secret-value",
    ] {
        assert!(!durable.contains(secret), "durable recipe leaked {secret}");
        assert!(
            !llm.contains(secret),
            "shape-only LLM payload leaked {secret}"
        );
        assert!(!matcher.contains(secret), "matcher payload leaked {secret}");
    }
}

#[tokio::test]
async fn invariant_08_origin_policy_ceiling_blocks_before_transport() {
    for mode in [
        OriginReplayMode::ObserveOnly,
        OriginReplayMode::ValidateOnly,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        policy
            .set_replay_mode("https://example.test", mode)
            .unwrap();
        let sends = Arc::new(AtomicUsize::new(0));
        let mut recipe = recipe(SideEffects::ReadOnly, "https://example.test/api/read");
        let result = run(
            &mut recipe,
            CountingTransport {
                sends: Arc::clone(&sends),
                response: Ok(response(200, "{\"ok\":true}")),
            },
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
        )
        .await;
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(result.fallback.unwrap().class, FailureClass::PolicyBlocked);
    }
}

#[tokio::test]
async fn invariant_09_auth_healing_never_exceeds_the_run_cap() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let healer = CountingHealer(AtomicUsize::new(0));
    let mut recipe = recipe(SideEffects::ReadOnly, "https://example.test/api/me");
    let result = run(
        &mut recipe,
        CountingTransport {
            sends,
            response: Ok(response(401, "expired")),
        },
        &grants,
        &policy,
        RecipeRunInputs::default(),
        Some(&healer),
        1,
    )
    .await;
    assert!(!result.success);
    assert_eq!(healer.0.load(Ordering::SeqCst), 1);
    assert_eq!(result.auth_heals, 1);
}

#[tokio::test]
async fn invariant_09b_run_outcomes_do_not_persist_raw_response_previews() {
    let temp = tempfile::tempdir().unwrap();
    let grants = ReplayGrantStore::open(temp.path());
    let policy = OriginPolicyStore::open(temp.path());
    let sends = Arc::new(AtomicUsize::new(0));
    let mut recipe = recipe(SideEffects::ReadOnly, "https://example.test/api/me");
    let result = run(
        &mut recipe,
        CountingTransport {
            sends,
            response: Ok(response(
                200,
                "{\"ok\":true,\"access_token\":\"response-secret-value\"}",
            )),
        },
        &grants,
        &policy,
        RecipeRunInputs::default(),
        None,
        0,
    )
    .await;
    assert!(result.success);
    assert!(!serde_json::to_string(&result)
        .unwrap()
        .contains("response-secret-value"));
}

#[test]
fn invariant_10_write_recipes_never_enter_the_fuzzy_match_rung() {
    let write = recipe(SideEffects::Write, "https://example.test/api/cart");
    let read = recipe(SideEffects::ReadOnly, "https://example.test/api/read");
    assert!(!eligible_for_fuzzy(&write));
    assert!(eligible_for_fuzzy(&read));
}

#[tokio::test]
async fn invariant_10b_task_binding_reextracts_the_edited_value() {
    let temp = tempfile::tempdir().unwrap();
    let store = RecipeStore::new(temp.path().to_path_buf());
    let mut write = recipe(SideEffects::Write, "https://example.test/api/cart");
    write.shape.template = "add {item} to cart".into();
    // Model a newly compiled recipe with an explicitly bound description,
    // not a legacy title-only record that cannot safely accept task edits.
    write.shape.description_template = Some(String::new());
    write.shape.inputs = vec![TaskInput {
        name: "item".into(),
        schema: TaskInputSchema::String,
        example_value: "apples".into(),
        source: TaskInputSource::TaskText,
    }];
    {
        let step = &mut write.current_mut().unwrap().steps[0];
        step.body_template = Some(r#"{"item":"{item}"}"#.into());
        step.param_sources.clear();
        step.param_sources.insert(
            "item".into(),
            magician::magician_v2::api_mining::recipe::RecipeParamSource::TaskInput {
                name: "item".into(),
            },
        );
    }
    store.save_and_bind(&write, "task_contract").unwrap();
    let matched = RecipeMatcher::deterministic(&store)
        .find(&TaskShapeQuery {
            task_id: "task_contract",
            title: "add oranges to cart",
            description: "",
            agent_id: "personal-assistant",
            principal: "anonymous",
            workspace: "default",
        })
        .await
        .expect("edited task should keep the deterministic binding");
    assert_eq!(
        matched.inputs.get("item").map(String::as_str),
        Some("oranges")
    );
    assert!(RecipeMatcher::deterministic(&store)
        .find(&TaskShapeQuery {
            task_id: "task_contract",
            title: "add oranges to cart",
            description: "Only use the organic supplier",
            agent_id: "personal-assistant",
            principal: "anonymous",
            workspace: "default",
        })
        .await
        .is_none());

    let mut read = write.clone();
    read.id = "rcp_read_contract".into();
    {
        let step = &mut read.current_mut().unwrap().steps[0];
        step.side_effects = SideEffects::ReadOnly;
        step.method = "GET".into();
        step.url_template = "https://example.test/api/cart?item={item}".into();
        step.body_template = None;
        step.verify_with = None;
    }
    store.save_and_bind(&read, "task_read_contract").unwrap();
    let unrelated = RecipeMatcher::deterministic(&store)
        .find(&TaskShapeQuery {
            task_id: "task_read_contract",
            title: "cancel my account",
            description: "",
            agent_id: "personal-assistant",
            principal: "anonymous",
            workspace: "default",
        })
        .await;
    assert!(
        unrelated.is_none(),
        "task-id binding must not reuse stale examples"
    );
}

#[test]
fn invariant_12_process_and_scope_switches_fail_closed_and_clear_live() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let switch = ApiMiningSwitch::new(true, move |principal, workspace| {
        root.join(principal).join(workspace)
    });
    assert!(switch.effective("anonymous", "default"));
    switch
        .set_scope("anonymous", "default", Some(false))
        .unwrap();
    assert!(!switch.effective("anonymous", "default"));
    switch.set_process_enabled(false);
    switch
        .set_scope("anonymous", "default", Some(true))
        .unwrap();
    assert!(!switch.effective("anonymous", "default"));
    switch.set_process_enabled(true);
    assert!(switch.effective("anonymous", "default"));
    switch.set_scope("anonymous", "default", None).unwrap();
    assert_eq!(switch.state("anonymous", "default").set_by, "default");
}
