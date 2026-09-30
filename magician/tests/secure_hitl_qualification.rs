//! Secure HITL qualification lane (plan §8, P7 Tasks 7.1–7.2).
//!
//! Each journey drives the runtime the way a person and a model would — the
//! API creates and starts an execution, a scripted model decides, the person
//! answers through `POST /hitl/{id}/respond`, the adapter delivers to the
//! fixture login service — and then proves two things separately: the
//! destination received the secret (the service's own receipt), and nothing
//! else did (a sweep of every file the runtime wrote, every realtime event it
//! published, and the execution record, for the canaries in every encoding).
//!
//! Run: `make test-secure-hitl-qualification`.
#[path = "support/secure_hitl_fixture.rs"]
mod fixture;
#[path = "support/v2_api_harness.rs"]
mod harness;

use std::time::{Duration, Instant};

use actix_web::{test, web, App, HttpRequest};
use magician::magician_v2::{
    execution::agentic::native_types::{ExecutionNativeResponse, ExecutionToolCall},
    realtime_events::RuntimeTransportEvent,
    storage::WaitingState,
};
use magician_api::{create_agent_definition_handler, web_api::respond_hitl_handler, MagicianV2Api};
use serde_json::{json, Value};

use fixture::{
    assert_clean, files_under, sweep_bytes, sweep_tree, Canaries, LoginService, ServiceMode,
};
use harness::*;

fn tool_call(id: &str, name: &str, arguments: Value) -> ExecutionNativeResponse {
    let mut arguments = arguments;
    if let Value::Object(map) = &mut arguments {
        map.entry("task_state_action")
            .or_insert(json!({"action": "none", "reason": "qualification journey"}));
    }
    let mut response = ExecutionNativeResponse::from_tool_calls(vec![ExecutionToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }]);
    response.finish_reason = Some("tool_calls".to_string());
    response
}

fn ask(input_type: &str, question: &str) -> ExecutionNativeResponse {
    tool_call(
        &format!("ask-{input_type}"),
        "need_user_input",
        json!({"thinking": "The service needs a credential the person holds.", "question": question, "input_type": input_type}),
    )
}

fn http_post(id: &str, url: &str, body: Value) -> ExecutionNativeResponse {
    tool_call(
        id,
        "http",
        json!({
            "thinking": "Deliver the credential to the service.",
            "method": "POST",
            "url": url,
            "body": body.to_string(),
            "content_type": "application/json",
            "timeout": 10,
        }),
    )
}

fn http_get(id: &str, url: &str, headers: Value) -> ExecutionNativeResponse {
    tool_call(
        id,
        "http",
        json!({
            "thinking": "Read the private page.",
            "method": "GET",
            "url": url,
            "headers": headers,
            "timeout": 10,
        }),
    )
}

fn done(evidence: &str) -> ExecutionNativeResponse {
    tool_call(
        "done",
        "goal_reached",
        json!({"evidence": format!("COMPLETED: {evidence}\nBLOCKED: none")}),
    )
}

/// Every event the broadcaster publishes from now on, collected on a task.
struct EventTap {
    events: std::sync::Arc<std::sync::Mutex<Vec<RuntimeTransportEvent>>>,
    api: web::Data<MagicianV2Api>,
}

impl EventTap {
    fn start(api: &web::Data<MagicianV2Api>) -> Self {
        let broadcaster = api
            .orchestrator()
            .event_broadcaster()
            .expect("the harness installs a broadcaster");
        let mut receiver = broadcaster.subscribe();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&events);
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => sink.lock().unwrap().push(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Self {
            events,
            api: api.clone(),
        }
    }

    fn snapshot(&self) -> Vec<RuntimeTransportEvent> {
        self.events.lock().unwrap().clone()
    }

    /// The first `hitl.requested` for `execution_id` whose spec kind is
    /// `kind` and whose correlation is not in `except`, within `timeout`:
    /// `(correlation_id, input_schema)`.
    async fn wait_hitl_requested(
        &self,
        execution_id: &str,
        kind: &str,
        except: &[String],
        timeout: Duration,
    ) -> (String, Value) {
        let deadline = Instant::now() + timeout;
        loop {
            for event in self.snapshot() {
                if let RuntimeTransportEvent::HitlRequested {
                    correlation_id,
                    execution_id: Some(id),
                    input_schema,
                    ..
                } = &event
                {
                    let schema = input_schema.clone().unwrap_or(Value::Null);
                    if id == execution_id
                        && schema["sensitive"]["kind"] == kind
                        && !except.contains(correlation_id)
                    {
                        return (correlation_id.clone(), schema);
                    }
                }
            }
            if Instant::now() >= deadline {
                let execution = self.api.orchestrator().get_execution(execution_id).await;
                let turns = self
                    .api
                    .orchestrator()
                    .get_turns(
                        execution_id,
                        runtime_core::PaginationParams::default(),
                        None,
                    )
                    .await
                    .map(|page| serde_json::to_string(&page.items).unwrap_or_default())
                    .unwrap_or_else(|e| e);
                panic!(
                    "no hitl.requested ({kind}) for {execution_id}; events: {:?}; execution: {execution:?}; turns: {turns}",
                    self.kinds()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn kinds(&self) -> Vec<String> {
        let dump = std::env::temp_dir().join("secure-hitl-qualification-events.json");
        let _ = std::fs::write(&dump, self.serialized());
        self.snapshot()
            .iter()
            .map(|e| format!("{e:?}").chars().take(60).collect())
            .collect()
    }

    fn serialized(&self) -> Vec<u8> {
        serde_json::to_vec(&self.snapshot()).expect("events serialize")
    }
}

async fn wait_terminal(api: &MagicianV2Api, execution_id: &str, timeout: Duration) -> WaitingState {
    let deadline = Instant::now() + timeout;
    loop {
        let execution = api
            .orchestrator()
            .get_execution(execution_id)
            .await
            .expect("execution readable");
        if matches!(
            execution.waiting_state,
            WaitingState::Completed | WaitingState::Failed | WaitingState::Cancelled
        ) {
            return execution.waiting_state;
        }
        assert!(
            Instant::now() < deadline,
            "execution {execution_id} still {:?}",
            execution.waiting_state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

macro_rules! journey_app {
    ($api:expr) => {
        test::init_service(
            App::new()
                .app_data($api.clone())
                .route(
                    "/api/magician/v2/executions",
                    web::post().to(
                        |api: web::Data<MagicianV2Api>,
                         http_req: HttpRequest,
                         req: web::Json<magician_api::CreateExecutionRequest>| async move {
                            api.create_execution(&http_req, req).await
                        },
                    ),
                )
                .route(
                    "/api/magician/v2/executions/{id}/start",
                    web::post().to(
                        |api: web::Data<MagicianV2Api>,
                         path: web::Path<String>,
                         req: web::Json<magician_api::StartExecutionRequest>| async move {
                            api.start_execution(path, req).await
                        },
                    ),
                )
                .route(
                    "/api/magician/v2/hitl/{correlation_id}/respond",
                    web::post().to(respond_hitl_handler),
                )
                .route(
                    "/api/magician/v2/agents",
                    web::post().to(create_agent_definition_handler),
                ),
        )
        .await
    };
}

/// One journey's runtime: the API, the scripted model, the fixture service,
/// the event tap, and the storage root the sweep reads.
struct Journey {
    api: web::Data<MagicianV2Api>,
    service: LoginService,
    canaries: Canaries,
    storage_path: String,
    tap: EventTap,
    answered: std::sync::Mutex<Vec<String>>,
    projector: tokio_util::sync::CancellationToken,
}

impl Drop for Journey {
    fn drop(&mut self) {
        self.projector.cancel();
    }
}

impl Journey {
    async fn start(
        mode: ServiceMode,
        script: impl FnOnce(&LoginService, &Canaries) -> Vec<ExecutionNativeResponse>,
    ) -> Self {
        // Runtime warnings and errors are the only trace of a run that never
        // reaches its ask; keep them visible under `--nocapture`.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
        let canaries = Canaries::generate();
        let service = LoginService::start(canaries.clone(), mode).await;
        let storage_path = unique_integration_storage_path();
        // The factory's service is a cutover-complete deployment (the
        // stateless driver refuses an unsealed scope) — see the seal in
        // `support/v2_api_harness.rs`.
        let api = create_test_v2_api_with_storage_path(storage_path.clone()).await;
        api.orchestrator()
            .set_test_native_responses(script(&service, &canaries));
        let tap = EventTap::start(&api);
        let projector = spawn_terminal_outbox_projector(&api);
        Self {
            api,
            service,
            canaries,
            storage_path,
            tap,
            answered: std::sync::Mutex::new(Vec::new()),
            projector,
        }
    }

    /// Create and start an execution through the API's own path, under the
    /// integration scope. The scope's owner agent — `personal-assistant`,
    /// which a deployment seeds per scope — is created first, without
    /// approval rules: the journeys are about the credential boundary, not
    /// the trust policy. It declares the `http` tool the journeys dispatch
    /// (an agent with no resolvable tool has an empty dispatch ceiling, and
    /// the universal packs ride only on a non-empty one).
    async fn run(&self, goal: &str) -> String {
        self.run_as("personal-assistant", json!({}), goal).await
    }

    /// The same, with the scope's owner agent seeded from `extra` — a parent
    /// that delegates needs its `delegation_targets`.
    async fn run_as(&self, owner: &str, extra: Value, goal: &str) -> String {
        self.seed_agent_with(owner, json!(["http"]), extra).await;
        let app = journey_app!(self.api);
        let create = with_integration_scope(test::TestRequest::post())
            .uri("/api/magician/v2/executions")
            .set_json(json!({}))
            .to_request();
        let created_response = test::call_service(&app, create).await;
        let created_status = created_response.status();
        let created: Value = test::read_body_json(created_response).await;
        let execution_id = created["execution_id"]
            .as_str()
            .unwrap_or_else(|| panic!("create execution: {created_status} {created}"))
            .to_string();
        let start = with_integration_scope(test::TestRequest::post())
            .uri(&format!("/api/magician/v2/executions/{execution_id}/start"))
            .set_json(json!({"goal": goal}))
            .to_request();
        let started = test::call_service(&app, start).await;
        assert_eq!(started.status(), actix_web::http::StatusCode::ACCEPTED);
        execution_id
    }

    /// Create one agent in the journey's scope, without approval rules: the
    /// journeys are about the credential boundary, not the trust policy. Its
    /// `tools` must name something resolvable — an agent with no resolvable
    /// tool has an empty dispatch ceiling, and the universal packs ride only
    /// on a non-empty one.
    async fn seed_agent(&self, agent_id: &str, tools: Value) {
        self.seed_agent_with(agent_id, tools, json!({})).await;
    }

    /// `extra` fields are merged into the definition — `delegation_targets`
    /// for a parent that hands work to another agent (the default is an empty
    /// list, which permits no delegation at all).
    async fn seed_agent_with(&self, agent_id: &str, tools: Value, extra: Value) {
        let app = journey_app!(self.api);
        let mut payload = json!({
            "agent_id": agent_id,
            "name": agent_id,
            "persona": format!("The qualification lane's `{agent_id}`."),
            "principal": INTEGRATION_PRINCIPAL,
            "workspace": INTEGRATION_WORKSPACE,
            "tools": tools,
            "trust_level": "reviewed",
        });
        if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                payload.insert(key.clone(), value.clone());
            }
        }
        let request = with_integration_scope(test::TestRequest::post())
            .uri("/api/magician/v2/agents")
            .set_json(payload)
            .to_request();
        let response = test::call_service(&app, request).await;
        let status = response.status();
        let body: Value = test::read_body_json(response).await;
        assert!(
            status.is_success() || status == actix_web::http::StatusCode::CONFLICT,
            "seeding {agent_id}: {status} {body}"
        );
    }

    /// Wait for the run's next ask of `kind`, answer it with `value` through
    /// the canonical respond endpoint, and check the reply is value-free.
    async fn answer(&self, execution_id: &str, kind: &str, value: &str) -> (String, Value) {
        let answered = self.answered.lock().unwrap().clone();
        let (correlation_id, schema) = self
            .tap
            .wait_hitl_requested(execution_id, kind, &answered, Duration::from_secs(30))
            .await;
        assert_eq!(schema["sensitive"]["kind"], kind);
        self.answered.lock().unwrap().push(correlation_id.clone());
        let app = journey_app!(self.api);
        let respond = with_integration_scope(test::TestRequest::post())
            .uri(&format!("/api/magician/v2/hitl/{correlation_id}/respond"))
            .set_json(json!({
                "source": "agentic",
                "execution_id": execution_id,
                "input_type": kind,
                "value": {"type": "password", "value": value},
                "channel": "web",
            }))
            .to_request();
        let responded = test::call_service(&app, respond).await;
        let status = responded.status();
        let body: Value = test::read_body_json(responded).await;
        assert!(status.is_success(), "respond ({kind}): {status} {body}");
        assert!(
            !body.to_string().contains(value),
            "the respond reply echoed the value: {body}"
        );
        (correlation_id, schema)
    }

    /// Whether the run told the model `needle` — read from the prompts the
    /// scripted model was actually asked. A refusal decided before dispatch
    /// reaches the model as its next tool result and is persisted nowhere
    /// else, so the prompt is the only honest place to prove it was given.
    fn the_model_was_told(&self, needle: &str) -> bool {
        self.api
            .orchestrator()
            .test_native_response_queue()
            .map(|queue| queue.prompts().iter().any(|prompt| prompt.contains(needle)))
            .unwrap_or(false)
    }

    /// The run finished as `Completed`.
    async fn finished(&self, execution_id: &str) {
        let terminal = wait_terminal(&self.api, execution_id, Duration::from_secs(40)).await;
        let kinds = self.tap.kinds();
        assert_eq!(terminal, WaitingState::Completed, "events: {kinds:?}");
    }

    /// No secret in any file the runtime wrote, any event it published, or
    /// the execution record.
    async fn sweep(&self, execution_id: &str, secrets: &[&str]) {
        // A sweep over the wrong root, or over a tree the run never wrote,
        // passes vacuously — so prove the tree being swept is this run's
        // before believing it is clean. The pause checkpoints, the loop
        // journal and the execution's own directory all live under here.
        let root = std::path::Path::new(&self.storage_path);
        let own = files_under(root, execution_id);
        assert!(
            !own.is_empty(),
            "the sweep found no file of execution {execution_id} under {}: it would pass on an empty tree",
            root.display()
        );
        assert_clean("the runtime's storage root", &sweep_tree(root, secrets));
        assert_clean(
            "the realtime event stream",
            &sweep_bytes("events", &self.tap.serialized(), secrets),
        );
        let execution = self
            .api
            .orchestrator()
            .get_execution(execution_id)
            .await
            .unwrap();
        let record = serde_json::to_vec(&execution).unwrap();
        assert_clean(
            "the execution record",
            &sweep_bytes("execution", &record, secrets),
        );
    }

    /// Submit an answer for `correlation_id` without asserting success: the
    /// status and body, for the journeys that prove a submission is refused.
    async fn respond(
        &self,
        execution_id: &str,
        correlation_id: &str,
        kind: &str,
        value: &str,
        scope: Option<(&str, &str)>,
    ) -> (actix_web::http::StatusCode, Value) {
        let app = journey_app!(self.api);
        let mut request = test::TestRequest::post()
            .uri(&format!("/api/magician/v2/hitl/{correlation_id}/respond"));
        let (principal, workspace) =
            scope.unwrap_or((INTEGRATION_PRINCIPAL, INTEGRATION_WORKSPACE));
        request = request
            .insert_header(("X-Principal", principal))
            .insert_header(("X-Workspace", workspace));
        let responded = test::call_service(
            &app,
            request
                .set_json(json!({
                    "source": "agentic",
                    "execution_id": execution_id,
                    "input_type": kind,
                    "value": {"type": "password", "value": value},
                    "channel": "web",
                }))
                .to_request(),
        )
        .await;
        let status = responded.status();
        let body: Value = test::read_body_json(responded).await;
        assert!(
            !body.to_string().contains(value),
            "a reply echoed the value: {body}"
        );
        (status, body)
    }

    /// The next ask of `kind` that has not been answered yet, without
    /// answering it.
    async fn next_ask(&self, execution_id: &str, kind: &str) -> (String, Value) {
        let answered = self.answered.lock().unwrap().clone();
        self.tap
            .wait_hitl_requested(execution_id, kind, &answered, Duration::from_secs(30))
            .await
    }

    /// The next ask of `kind` anywhere in this scope — a delegated child's
    /// run included — with the execution that raised it.
    async fn next_ask_anywhere(&self, kind: &str) -> (String, String) {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let answered = self.answered.lock().unwrap().clone();
            for event in self.tap.snapshot() {
                if let RuntimeTransportEvent::HitlRequested {
                    correlation_id,
                    execution_id: Some(execution_id),
                    input_schema,
                    ..
                } = &event
                {
                    let schema = input_schema.clone().unwrap_or(Value::Null);
                    if schema["sensitive"]["kind"] == kind && !answered.contains(correlation_id) {
                        return (execution_id.clone(), correlation_id.clone());
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "no {kind} ask in this scope: {:?}",
                self.tap.kinds()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn resolved_on_the_feed(&self, correlation_id: &str) -> bool {
        self.tap
            .snapshot()
            .iter()
            .any(|e| matches!(e, RuntimeTransportEvent::HitlResolved { correlation_id: c, .. } if c == correlation_id))
    }
}

/// §8 "Login needs username and password" (password through the generic
/// pause), delivered by the HTTP adapter: one ask, one custody, one delivery
/// to the service, and the password absent from every storage class, event
/// and record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_password_answered_through_the_api_reaches_only_the_login_service() {
    let journey = Journey::start(ServiceMode::Accept, |service, canaries| {
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            done("logged in to the fixture service"),
        ]
    })
    .await;
    let execution_id = journey
        .run("Sign in to the fixture service with the password the person provides")
        .await;
    let (correlation_id, schema) = journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    assert!(schema.to_string().contains("sensitive"), "{schema}");
    journey.finished(&execution_id).await;

    // Destination receipt, from the service's own record.
    let logins = journey.service.posted_to("/login").await;
    assert_eq!(logins.len(), 1, "one delivery: {logins:?}");
    let posted: Value = serde_json::from_str(&logins[0]).unwrap();
    assert_eq!(posted["username"], journey.canaries.identifier);
    assert_eq!(
        posted["password"], journey.canaries.password,
        "the reference was lowered at the sink, not sent as a marker"
    );

    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
    assert!(
        journey.resolved_on_the_feed(&correlation_id),
        "the ask resolved on the feed"
    );
}

/// §5.1 / §8: a `401` with `WWW-Authenticate` from the service binds the
/// next password ask to that origin; the bound password is delivered in the
/// header, to that origin only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_challenge_binds_the_password_to_the_origin_that_raised_it() {
    let journey = Journey::start(ServiceMode::Accept, |service, _| {
        let private = format!("{}/api/private", service.origin());
        vec![
            http_get("probe", &private, json!({})),
            ask("password", "The service asks for your password"),
            http_get(
                "authorized",
                &private,
                json!({"Authorization": "Basic [REF:password]"}),
            ),
            done("read the private page"),
        ]
    })
    .await;
    let execution_id = journey
        .run("Read the private page of the fixture service")
        .await;
    let (_, schema) = journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    assert_eq!(
        schema["sensitive"]["expected_destination"],
        journey.service.origin(),
        "the ask is bound to the challenging origin: {schema}"
    );
    assert!(
        schema["sensitive"]["challenge_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("password:")),
        "{schema}"
    );
    journey.finished(&execution_id).await;

    let receipts = journey.service.receipts().await;
    let private: Vec<_> = receipts
        .iter()
        .filter(|(m, p, _)| m == "GET" && p == "/api/private")
        .collect();
    assert_eq!(
        private.len(),
        2,
        "the probe and the authorized read: {receipts:?}"
    );
    // The header sink's own receipt. Absence everywhere else means nothing
    // unless the destination actually received the credential — redaction
    // must not be able to pass as a working login (§8).
    let authorization = journey.service.header_values("authorization").await;
    assert_eq!(
        authorization,
        vec![format!("Basic {}", journey.canaries.password)],
        "the bound password reached the origin in the header it was written into"
    );
    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
}

/// The re-ask journey is the deepest stack the runtime reaches: a run that
/// pauses to ask again commits its pause ~100 frames down, inside the loop
/// that is already running the iteration. On 2026-09-23 that path measured
/// 1979 KiB against a tokio worker's 2048 KiB, and the process died with
/// `thread 'tokio-rt-worker' has overflowed its stack` — after every test in
/// this file had printed `ok`, which is how a SIGABRT here reads as a pass.
/// Boxing the two deepest pause-tail awaits brought it to ~1735 KiB.
///
/// This test pins the budget explicitly rather than inheriting it from
/// tokio's default, and gives the number above somewhere to live. An overflow
/// still aborts the process — a guard-page fault cannot be caught and reported
/// as a failed assertion — but it aborts inside a test named for the budget it
/// spent, which the journey tests it hid behind were not. Verified to trip: at
/// half this stack the path overflows. Raising `RUST_MIN_STACK` is not the
/// remedy — the Makefile unsets it and `agentic_default_stack_contract`
/// asserts it stays unset. This path is contracted to fit an ordinary worker.
// Fully qualified: `use actix_web::test` shadows the built-in attribute in
// this file, and actix's own `#[test]` demands an async fn.
#[::core::prelude::v1::test]
fn default_stack_a_pause_commit_fits_an_ordinary_worker() {
    const ORDINARY_WORKER_STACK: usize = 2 * 1024 * 1024;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(ORDINARY_WORKER_STACK)
        .enable_all()
        .build()
        .expect("default-stack guard runtime");
    runtime.block_on(a_rejected_code_is_never_replayed_and_a_fresh_ask_follows_body());
}

/// §4 / §8 "Destination rejects a code": the code the person typed is
/// consumed by one bound submission; when the service rejects it, the run
/// asks for a fresh code and the old one is never replayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_code_is_never_replayed_and_a_fresh_ask_follows() {
    a_rejected_code_is_never_replayed_and_a_fresh_ask_follows_body().await
}

async fn a_rejected_code_is_never_replayed_and_a_fresh_ask_follows_body() {
    let journey = Journey::start(ServiceMode::RejectFirstCode, |service, canaries| {
        let otp = format!("{}/otp", service.origin());
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            ask("otp", "Enter the code the service sent"),
            http_post("code-1", &otp, json!({"code": "[REF:otp]"})),
            ask("otp", "The service rejected that code; enter the fresh one"),
            http_post("code-2", &otp, json!({"code": "[REF:otp]"})),
            done("logged in with the fresh code"),
        ]
    })
    .await;
    let execution_id = journey
        .run("Sign in to the fixture service and complete the code step")
        .await;
    journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    let (first, _) = journey
        .answer(&execution_id, "otp", &journey.canaries.code)
        .await;
    let (second, _) = journey
        .answer(&execution_id, "otp", &journey.canaries.code)
        .await;
    assert_ne!(first, second, "a fresh ask, not the old one re-opened");
    journey.finished(&execution_id).await;

    let codes = journey.service.posted_to("/otp").await;
    assert_eq!(codes.len(), 2, "one submission per ask: {codes:?}");
    for body in &codes {
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap()["code"],
            journey.canaries.code
        );
    }
    journey
        .sweep(
            &execution_id,
            &[
                journey.canaries.password.as_str(),
                journey.canaries.code.as_str(),
            ],
        )
        .await;
}

/// §8 "Two submissions or two consumers race": the second submission of the
/// same ask finds the pause consumed and is refused; the destination is
/// delivered to exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_submission_of_one_ask_is_refused_and_the_service_is_paid_once() {
    let journey = Journey::start(ServiceMode::Accept, |service, canaries| {
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            done("logged in once"),
        ]
    })
    .await;
    let execution_id = journey.run("Sign in to the fixture service").await;
    let (correlation_id, _) = journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    // The same person's second click, or a second surface's submit.
    let (status, body) = journey
        .respond(
            &execution_id,
            &correlation_id,
            "password",
            &journey.canaries.password,
            None,
        )
        .await;
    assert!(
        matches!(status.as_u16(), 404 | 409 | 410),
        "a replay must be refused as gone or already resolved, not {status}: {body}"
    );
    journey.finished(&execution_id).await;

    let logins = journey.service.posted_to("/login").await;
    assert_eq!(logins.len(), 1, "one bound submission: {logins:?}");
    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
}

/// §8 "Scope … changes": an answer submitted under another scope is refused
/// before custody, the ask stays open, and the right scope still answers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_from_another_scope_is_refused_and_the_ask_stays_open() {
    let journey = Journey::start(ServiceMode::Accept, |service, canaries| {
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            done("logged in after the wrong scope was refused"),
        ]
    })
    .await;
    let execution_id = journey.run("Sign in to the fixture service").await;
    let (correlation_id, _) = journey.next_ask(&execution_id, "password").await;
    let (status, body) = journey
        .respond(
            &execution_id,
            &correlation_id,
            "password",
            &journey.canaries.password,
            Some(("someone-else", "their-workspace")),
        )
        .await;
    assert!(
        matches!(status.as_u16(), 403 | 404 | 410),
        "another scope's answer must be refused on scope or absence, not {status}: {body}"
    );
    assert!(
        journey.service.receipts().await.is_empty(),
        "the refused answer reached the service"
    );

    let (again, _) = journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    assert_eq!(again, correlation_id, "the same ask was still open");
    journey.finished(&execution_id).await;
    assert_eq!(journey.service.posted_to("/login").await.len(), 1);
    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
}

/// §8 "stale-answer acceptance" with the P7 ask identity: once the run has
/// moved on to its next question, the earlier ask's id is refused instead of
/// answering the question now open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_to_a_superseded_ask_never_answers_the_open_one() {
    let journey = Journey::start(ServiceMode::RejectFirstCode, |service, _canaries| {
        let otp = format!("{}/otp", service.origin());
        vec![
            ask("otp", "Enter the code the service sent"),
            http_post("code-1", &otp, json!({"code": "[REF:otp]"})),
            ask("otp", "The service rejected that code; enter the fresh one"),
            http_post("code-2", &otp, json!({"code": "[REF:otp]"})),
            done("logged in with the fresh code"),
        ]
    })
    .await;
    let execution_id = journey
        .run("Complete the code step at the fixture service")
        .await;
    let (first, _) = journey
        .answer(&execution_id, "otp", &journey.canaries.code)
        .await;
    let (second, _) = journey.next_ask(&execution_id, "otp").await;
    assert_ne!(first, second, "the fresh ask is its own request");

    // The two asks of one step share a storage key, so this submission does
    // reach the pause now open — and is refused by the ask it names, with the
    // pause left untouched for the answer that belongs to it.
    let (status, body) = journey
        .respond(&execution_id, &first, "otp", &journey.canaries.code, None)
        .await;
    assert_eq!(status, actix_web::http::StatusCode::CONFLICT, "{body}");
    assert_eq!(body["details"]["reason"], "superseded_ask", "{body}");
    let (answered, _) = journey
        .answer(&execution_id, "otp", &journey.canaries.code)
        .await;
    assert_eq!(
        answered, second,
        "the open ask is the one that got the answer"
    );
    journey.finished(&execution_id).await;
    assert_eq!(
        journey.service.posted_to("/otp").await.len(),
        2,
        "one submission per ask"
    );
    journey
        .sweep(&execution_id, &[journey.canaries.code.as_str()])
        .await;
}

/// The compatibility half of the ask identity: a client that knows only the
/// pause's storage key (an older build, or a record written before asks had
/// identities) still answers the question that key holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_knows_only_the_pause_key_still_answers_the_ask() {
    let journey = Journey::start(ServiceMode::Accept, |service, canaries| {
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            done("logged in through the legacy selector"),
        ]
    })
    .await;
    let execution_id = journey.run("Sign in to the fixture service").await;
    let (correlation_id, _) = journey.next_ask(&execution_id, "password").await;
    let (key, ask_id) =
        magician::magician_v2::execution::agentic::split_hitl_correlation_id(&correlation_id);
    assert!(
        ask_id.is_some(),
        "the canonical id names its ask: {correlation_id}"
    );
    let (status, body) = journey
        .respond(
            &execution_id,
            key,
            "password",
            &journey.canaries.password,
            None,
        )
        .await;
    assert!(
        status.is_success(),
        "the bare key was refused: {status} {body}"
    );
    journey.finished(&execution_id).await;
    assert_eq!(journey.service.posted_to("/login").await.len(), 1);
    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
}

/// §5.2 / §8 "Unsafe action": the URL is not a credential sink. A dispatch
/// that puts the reference in the query is refused before it leaves, so the
/// service never sees the password in a request line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reference_in_the_url_is_refused_before_the_request_leaves() {
    let journey = Journey::start(ServiceMode::Accept, |service, _| {
        vec![
            ask("password", "Enter the password for the fixture service"),
            http_get(
                "leak",
                &format!("{}/api/private?token=[REF:password]", service.origin()),
                json!({}),
            ),
            done("the run stopped short of leaking the password in a URL"),
        ]
    })
    .await;
    let execution_id = journey
        .run("Read the private page of the fixture service")
        .await;
    journey
        .answer(&execution_id, "password", &journey.canaries.password)
        .await;
    journey.finished(&execution_id).await;

    for (method, line) in journey.service.request_lines().await {
        assert!(
            !line.contains(&journey.canaries.password),
            "the password left in a request line: {method} {line}"
        );
    }
    for (method, path, body) in journey.service.receipts().await {
        assert!(
            !body.contains(&journey.canaries.password),
            "the password left in a body: {method} {path}"
        );
    }
    // Absence is not the assertion: a sink walker that silently DROPPED the
    // substitution would leave the same trace. The dispatch must be refused
    // and the sink named — and the place that says so is the run's own
    // conversation, because the refusal reaches the model as its next tool
    // result. (It reaches no realtime event: an action refused before
    // dispatch publishes no `AgenticActionExecuted`, so the feed shows an
    // operator nothing. Recorded in the plan as an observability gap, not
    // asserted here as if it were surfaced.)
    assert!(
        journey.the_model_was_told("an HTTP URL or query string"),
        "the run was never told why the dispatch was refused; the model cannot correct a \
         refusal it was not given: {:?}",
        journey.tap.kinds()
    );
    journey
        .sweep(&execution_id, &[journey.canaries.password.as_str()])
        .await;
}

/// §6.2 / §8 "Code arrives in enabled authorized email account": the owner
/// permitted one account for verification codes, the code arrives there, and
/// the run's `otp` ask is answered by the runtime's own resolver — no person,
/// no model. The resolver is composed here the way `magician-bin` composes it
/// (registry over the scope's own `channel_observe` record, the API's agentic
/// answer sink, the broadcaster as both lifecycle source and status
/// transport), with a fake source watch standing in for the Gmail reader.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_code_in_a_permitted_inbox_answers_the_ask_with_no_person_involved() {
    use magician::magician_v2::verification_codes::{
        MessageEvidence, RetrievalStatus, RuntimeSourceRegistry, SourceKind, SourceWatch,
        VerificationCodeResolver, WatchPoll,
    };

    /// The permitted account's reader: it hands out the service's message
    /// once, then nothing.
    struct InboxWatch {
        code: String,
        handed_out: std::sync::atomic::AtomicBool,
        clock: i64,
    }

    #[async_trait::async_trait]
    impl SourceWatch for InboxWatch {
        fn kind(&self) -> SourceKind {
            SourceKind::Gmail
        }

        async fn poll(
            &self,
            source: &magician::magician_v2::verification_codes::AuthorizedSource,
            _challenge: &magician::magician_v2::verification_codes::ChallengeContext,
            _since_ms: i64,
            _limit: usize,
        ) -> Result<WatchPoll, String> {
            if self
                .handed_out
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(WatchPoll::messages(Vec::new()));
            }
            Ok(WatchPoll::messages(vec![MessageEvidence {
                source: SourceKind::Gmail,
                account: source.account.clone(),
                message_id: "fixture-code-1".to_string(),
                received_at_ms: self.clock,
                sender_address: Some("no-reply@fixture.test".to_string()),
                authenticated: Some(true),
                subject: Some("Your sign-in code".to_string()),
                body: format!(
                    "Your verification code is {}. It expires in 10 minutes.",
                    self.code
                ),
            }]))
        }
    }

    let journey = Journey::start(ServiceMode::Accept, |service, _| {
        vec![
            ask("otp", "Enter the code the service sent"),
            http_post(
                "code",
                &format!("{}/otp", service.origin()),
                json!({"code": "[REF:otp]"}),
            ),
            done("signed in with the retrieved code"),
        ]
    })
    .await;

    // The owner's grant: one enabled account with the verification-codes
    // purpose. Observation consent alone grants nothing (§6.2).
    let layout = std::sync::Arc::new(integration_workspace_layout(
        &journey.api.orchestrator().pause_states_storage_path(),
    ));
    let account = "owner@fixture.test";
    magician::magician_v2::observe_connectors::write_channel_observe(
        &layout,
        INTEGRATION_PRINCIPAL,
        INTEGRATION_WORKSPACE,
        &magician::magician_v2::observe_connectors::ChannelObserveConfig {
            channels: vec![magician::magician_v2::observe_connectors::ChannelEntry {
                channel: "email".to_string(),
                account: account.to_string(),
                lane: Default::default(),
                enabled: true,
                purposes: vec![
                    magician::magician_v2::observe_connectors::VERIFICATION_CODES_PURPOSE
                        .to_string(),
                ],
            }],
            ..Default::default()
        },
    )
    .await
    .expect("the owner's channel grant is recorded");

    let broadcaster = journey
        .api
        .orchestrator()
        .event_broadcaster()
        .expect("broadcaster");
    let resolver = std::sync::Arc::new(VerificationCodeResolver::with_clock(
        magician::config::HitlVerificationCodesSettings::default(),
        std::sync::Arc::new(RuntimeSourceRegistry::new(std::sync::Arc::clone(&layout))),
        std::sync::Arc::new(
            magician_api::verification_codes_api::ApiAgenticAnswerSink::new(
                journey.api.clone().into_inner(),
            ),
        ),
        std::sync::Arc::clone(&broadcaster)
            as std::sync::Arc<dyn magician::magician_v2::verification_codes::StatusTransport>,
        std::sync::Arc::new(magician::magician_v2::verification_codes::SystemClock),
        // The production interval is four seconds; a journey should not wait
        // it out to prove the loop runs.
        Duration::from_millis(100),
    ));
    resolver
        .register_watch(std::sync::Arc::new(InboxWatch {
            code: journey.canaries.code.clone(),
            handed_out: std::sync::atomic::AtomicBool::new(false),
            clock: chrono::Utc::now().timestamp_millis(),
        }))
        .await;
    std::sync::Arc::clone(&resolver).start(std::sync::Arc::clone(&broadcaster));

    let execution_id = journey
        .run("Complete the code step at the fixture service")
        .await;
    // Nobody answers through the API: the next thing that happens is the
    // run's own dispatch of the retrieved code.
    let (correlation_id, _) = journey.next_ask(&execution_id, "otp").await;
    journey.finished(&execution_id).await;

    let codes = journey.service.posted_to("/otp").await;
    assert_eq!(
        codes.len(),
        1,
        "one submission of the retrieved code: {codes:?}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&codes[0]).unwrap()["code"],
        journey.canaries.code,
        "the code the inbox carried is the code the service received"
    );
    // The status is the surface's projection of the watch, and the watch
    // records `code_used` only once the ask's own answer path has returned —
    // which, on the stateless driver, is after the resumed run has parked or
    // ended. So the run finishing first is the normal order, not a failure.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = resolver
            .status(
                INTEGRATION_PRINCIPAL,
                INTEGRATION_WORKSPACE,
                &correlation_id,
            )
            .await
            .expect("the resolver kept this challenge's status readable");
        if status.status != RetrievalStatus::Waiting || Instant::now() >= deadline {
            break status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(status.status, RetrievalStatus::CodeUsed, "{status:?}");
    assert_eq!(
        status.sources,
        vec!["gmail".to_string()],
        "the source it read: {status:?}"
    );
    assert!(
        journey.resolved_on_the_feed(&correlation_id),
        "the ask resolved on the feed"
    );
    journey
        .sweep(&execution_id, &[journey.canaries.code.as_str()])
        .await;
}

/// The matrix's "delegated child" row, which P3 could only verify by reading:
/// a child run raises the password ask, the person answers it through the
/// canonical endpoint, the child delivers to the service and reports back,
/// and the parent completes — with the password absent from the parent's
/// record and from every event either run published. The child's pause is its
/// own record with its own spec; the parent only ever holds references.
///
/// # Ignored: it reproduces a defect outside this program's boundary
///
/// The journey gets as far as the ask and stops there, because **a delegated
/// child's question never reaches the canonical HITL feed**. What the runtime
/// does (evidence from a run of this fixture, 2026-09-22):
///
/// * the parent delegates and goes `WaitingChildren`; the child run starts
///   and decides `need_user_input`; the child's own journal records
///   `HitlRequested` (seq 12) and its run ends terminal `waiting_for_user`
///   with a `terminal_settlement_receipt` in its committed loop state —
///   everything the terminal-outbox projector needs;
/// * but the run's terminal-debt catalog stays `complete` and **empty**, so
///   the projector discovers nothing, no `hitl.requested` is published, and
///   no surface — feed, attention, chat, delivery channel — ever shows the
///   question. A parent's own pause in the same fixture is discovered and
///   projected normally, and a resumed segment's terminal does get a catalog
///   entry, so the gap is specific to the delegated child's terminal;
/// * ~30 s later the delegation's own timeout cancels the child
///   (`WaitingUser` → `Cancelled`), so the person is never asked and the
///   parent's work dies with it.
///
/// That is the loop-state discovery contract, not the credential boundary:
/// the password never leaks (there is nothing to answer), and every
/// credential assertion below is still the one this lane should make once a
/// child's ask surfaces. Kept as the failing fixture that fix must turn
/// green — the same discipline P0 used for its red fixtures — rather than
/// deleted or weakened into a passing test of something else.
#[ignore = "reproduces a pre-existing gap: a delegated child's hitl.requested is never projected (see the comment)"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegated_child_collects_the_password_and_the_parent_never_holds_it() {
    let journey = Journey::start(ServiceMode::Accept, |service, canaries| {
        vec![
            // The parent hands the work to the helper…
            tool_call(
                "delegate",
                "delegate_to_agent",
                json!({
                    "thinking": "The helper owns the sign-in.",
                    "delegation_targets": [{
                        "target_agent_id": "sign-in-helper",
                        "context": "Sign in to the fixture service with the password the person provides.",
                    }],
                }),
            ),
            // …the child asks, delivers and reports…
            ask("password", "Enter the password for the fixture service"),
            http_post(
                "login",
                &format!("{}/login", service.origin()),
                json!({"username": canaries.identifier, "password": "[REF:password]"}),
            ),
            done("the helper signed in"),
            // …and the parent concludes on the child's report.
            done("the helper reported a successful sign-in"),
        ]
    })
    .await;
    journey.seed_agent("sign-in-helper", json!(["http"])).await;
    // A parent delegates only to the agents its definition names: the default
    // is an empty list, which permits no delegation at all.
    let parent = journey
        .run_as(
            "personal-assistant",
            json!({"delegation_targets": ["sign-in-helper"]}),
            "Have the helper sign in to the fixture service",
        )
        .await;

    let (child, correlation_id) = journey.next_ask_anywhere("password").await;
    assert_ne!(child, parent, "the ask belongs to the child's own run");
    let (status, body) = journey
        .respond(
            &child,
            &correlation_id,
            "password",
            &journey.canaries.password,
            None,
        )
        .await;
    assert!(
        status.is_success(),
        "answering the child's ask: {status} {body}"
    );
    journey
        .answered
        .lock()
        .unwrap()
        .push(correlation_id.clone());

    journey.finished(&parent).await;
    let logins = journey.service.posted_to("/login").await;
    assert_eq!(logins.len(), 1, "the child delivered once: {logins:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&logins[0]).unwrap()["password"],
        journey.canaries.password
    );
    journey
        .sweep(&parent, &[journey.canaries.password.as_str()])
        .await;
    journey
        .sweep(&child, &[journey.canaries.password.as_str()])
        .await;
}
