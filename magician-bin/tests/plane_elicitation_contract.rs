//! Focused, provider-free contracts over the production form adapter and HTTP
//! door. This target deliberately avoids the monolith's test-fixtures feature.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use actix_web::{web, App, HttpResponse, HttpServer};
use magician::magician_v2::execution::agentic::types::{
    ChoiceOption, FormQuestion, UserInputType, UserInputValue,
};
use magician::magician_v2::execution::agentic::{ActionExecutors, AgenticContext};
use magician::magician_v2::execution::plane::input::{
    input_type_from_arguments, pause_input_revision, InputForm,
};
use magician::magician_v2::execution::plane::{
    plane_grant_registry, PlaneCatalogProfile, PlaneGrant,
};
use magician::magician_v2::prompts::json_storage::JsonStorageConfig;
use magician::magician_v2::prompts::{JsonPromptStorage, PromptManager};
use magician::magician_v2::slot_graph::extraction::{
    LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
};
use magician_api::plane_api::{plane_mcp_delete_handler, plane_mcp_handler, plane_mcp_sse_handler};
use serde_json::{json, Value};

fn sse_event(payload: &Value) -> String {
    format!("event: message\ndata: {payload}\n\n")
}

// Exercise the same private pending-call implementation without exposing test
// hooks or a public transport-state API in the production HTTP crate.
#[path = "../../magician-api/src/plane_api/elicitation.rs"]
mod elicitation;

fn options() -> Vec<ChoiceOption> {
    vec![
        ChoiceOption {
            id: "small".into(),
            label: "Small plan".into(),
            description: None,
        },
        ChoiceOption {
            id: "large".into(),
            label: "Large plan".into(),
            description: None,
        },
    ]
}

#[test]
fn typed_forms_round_trip_without_losing_ids_or_skipped_answers() {
    let cases = [
        (
            UserInputType::Text {
                placeholder: None,
                multiline: true,
            },
            json!({"value":"first\nsecond"}),
            json!({"type":"text","value":"first\nsecond"}),
        ),
        (
            UserInputType::Guidance {
                context: None,
                suggestions: None,
            },
            json!({"value":"Try the smaller plan"}),
            json!({"type":"guidance","advice":"Try the smaller plan"}),
        ),
        (
            UserInputType::Choice {
                options: options(),
                allow_other: false,
            },
            json!({"selected_id":"large"}),
            json!({"type":"choice","selected_id":"large","other_value":null}),
        ),
        (
            UserInputType::Choice {
                options: options(),
                allow_other: true,
            },
            json!({"selected_id":"other","other_value":"custom"}),
            json!({"type":"choice","selected_id":"other","other_value":"custom"}),
        ),
        (
            UserInputType::MultiChoice {
                options: options(),
                min_selections: 1,
                max_selections: 2,
            },
            json!({"selected_ids":["large","small"]}),
            json!({"type":"multi_choice","selected_ids":["large","small"]}),
        ),
        (
            UserInputType::Confirmation {
                confirm_label: None,
                deny_label: None,
                destructive: true,
            },
            json!({"confirm":false}),
            json!({"type":"confirmation","confirmed":false}),
        ),
    ];
    for (input_type, content, expected) in cases {
        let form = InputForm::new(input_type).unwrap();
        let value = form
            .decode(&json!({"action":"accept","content":content}))
            .unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
    }
    let form = InputForm::new(UserInputType::Form {
        questions: vec![
            FormQuestion {
                id: "name".into(),
                prompt: "Name".into(),
                input_type: "text".into(),
                options: vec![],
            },
            FormQuestion {
                id: "plan".into(),
                prompt: "Plan".into(),
                input_type: "choice".into(),
                options: options(),
            },
            FormQuestion {
                id: "extras".into(),
                prompt: "Extras".into(),
                input_type: "multi_choice".into(),
                options: options(),
            },
        ],
    })
    .unwrap();
    let value = form
        .decode(&json!({"action":"accept","content":{"name":"", "extras":[]}}))
        .unwrap();
    let UserInputValue::Form { answers } = value else {
        panic!("form answer expected")
    };
    assert_eq!(answers[0].value.as_deref(), Some(""));
    assert!(!answers[0].skipped);
    assert!(answers[1].skipped);
    assert!(answers[2].selected_ids.is_empty());
    assert!(!answers[2].skipped);
}

#[test]
fn forms_reject_bad_fields_options_limits_and_sensitive_types() {
    let choice = InputForm::new(UserInputType::Choice {
        options: options(),
        allow_other: true,
    })
    .unwrap();
    for content in [
        json!({}),
        json!({"selected_id":"forged"}),
        json!({"selected_id":12}),
        json!({"selected_id":"other"}),
        json!({"selected_id":"small","other_value":"forged"}),
        json!({"selected_id":"small","extra":true}),
    ] {
        assert!(
            choice
                .decode(&json!({"action":"accept","content":content}))
                .is_err(),
            "{content}"
        );
    }
    let multi = InputForm::new(UserInputType::MultiChoice {
        options: options(),
        min_selections: 1,
        max_selections: 1,
    })
    .unwrap();
    for content in [
        json!({"selected_ids":[]}),
        json!({"selected_ids":["small","small"]}),
        json!({"selected_ids":["small","large"]}),
        json!({"selected_ids":[true]}),
    ] {
        assert!(multi
            .decode(&json!({"action":"accept","content":content}))
            .is_err());
    }
    assert!(InputForm::new(UserInputType::Password { placeholder: None }).is_err());
    assert!(InputForm::new(UserInputType::MultiChoice {
        options: options(),
        min_selections: 2,
        max_selections: 1
    })
    .is_err());
    let mut duplicate = options();
    duplicate[1].id = duplicate[0].id.clone();
    assert!(InputForm::new(UserInputType::Choice {
        options: duplicate,
        allow_other: false
    })
    .is_err());
    assert!(choice.decode(&json!({"action":"accept"})).is_err());
    assert!(choice.decode(&json!({"action":"unexpected"})).is_err());
    for action in ["decline", "cancel"] {
        assert!(choice
            .decode(&json!({"action":action}))
            .unwrap()
            .is_aborted());
    }
}

#[test]
fn native_question_vocabulary_and_pause_revisions_are_preserved() {
    assert!(input_type_from_arguments(&json!({"input_type":{"type":"password"}})).is_err());
    assert!(input_type_from_arguments(&json!({"input_type":null})).is_err());
    let form = input_type_from_arguments(&json!({"input_type":"text", "questions":[
        {"id":"a","question":"First?"}, {"id":"b","prompt":"Second?","input_type":"text"}
    ]}))
    .unwrap();
    assert!(matches!(form, UserInputType::Form { .. }));
    let now = chrono::Utc::now();
    let a = pause_input_revision(&now, &form, Some("question"), Some("action-a"));
    assert_eq!(
        a,
        pause_input_revision(&now, &form, Some("question"), Some("action-a"))
    );
    assert_ne!(
        a,
        pause_input_revision(&now, &form, Some("question"), Some("action-b"))
    );
    assert_ne!(
        a,
        pause_input_revision(
            &(now + chrono::Duration::seconds(1)),
            &form,
            Some("question"),
            Some("action-a")
        )
    );
}

#[test]
fn delegated_input_follows_only_the_launched_roots_parent_edges() {
    use magician::magician_v2::execution::plane::run_ownership::input_execution_ids;
    let edges = vec![
        ("root".into(), None),
        ("child".into(), Some("root".into())),
        ("grandchild".into(), Some("child".into())),
        ("other-root".into(), None),
        ("other-child".into(), Some("other-root".into())),
        ("cycle-a".into(), Some("cycle-b".into())),
        ("cycle-b".into(), Some("cycle-a".into())),
    ];
    let ids = input_execution_ids("root", &edges).unwrap();
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        ["child", "grandchild", "root"]
    );
    assert!(input_execution_ids(
        "root",
        &[("x".into(), None), ("x".into(), Some("root".into()))]
    )
    .is_err());
}

#[tokio::test]
async fn pending_answers_are_session_bound_consumed_once_and_cleaned_on_drop() {
    let (call, mut messages) =
        elicitation::CallState::new("a".into(), json!(1), "request_user_input".into());
    let ask = call.ask("Name?", UserInputType::default());
    tokio::pin!(ask);
    let prompt = tokio::select! {
        message = messages.recv() => message.unwrap(),
        _ = &mut ask => panic!("answer required"),
    };
    let prompt: Value = serde_json::from_str(
        prompt
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .unwrap(),
    )
    .unwrap();
    let answer = json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"action":"accept","content":{"value":"Ada"}}});
    assert!(!call.answer("b", &answer));
    assert!(call.answer("a", &answer));
    assert!(!call.answer("a", &answer));
    assert!(matches!(ask.await.unwrap(), UserInputValue::Text { value } if value == "Ada"));
    let pending = call.ask("Another?", UserInputType::default());
    let mut pending = Box::pin(pending);
    tokio::select! { _ = messages.recv() => {}, _ = &mut pending => panic!("answer required") }
    drop(pending);
    // A service-side answer can drop the MCP ask. The next question must work.
    let next = call.ask("Next?", UserInputType::default());
    tokio::pin!(next);
    tokio::select! { _ = messages.recv() => {}, _ = &mut next => panic!("stale pending slot") }
    call.cancelled.cancel();
    assert!(next.await.is_err());
}

#[tokio::test(start_paused = true)]
async fn unanswered_prompt_times_out_without_a_resume_value() {
    let (call, mut messages) =
        elicitation::CallState::new("a".into(), json!(1), "request_user_input".into());
    let ask = call.ask("Name?", UserInputType::default());
    tokio::pin!(ask);
    tokio::select! { _ = messages.recv() => {}, _ = &mut ask => panic!("answer required") }
    tokio::time::advance(Duration::from_secs(301)).await;
    assert!(ask.await.unwrap_err().contains("timed out"));
}

struct NoLlm;
#[async_trait::async_trait]
impl LlmService for NoLlm {
    async fn call_function(
        &self,
        _: LlmFunctionCallRequest,
    ) -> anyhow::Result<LlmFunctionCallResponse> {
        panic!("plane elicitation contracts must not call an LLM")
    }
}

struct Door {
    url: String,
    token: String,
    grant: PlaneGrant,
    server: actix_web::dev::ServerHandle,
    _dir: tempfile::TempDir,
}

impl Door {
    async fn start() -> Self {
        Self::start_with_notifications(true).await
    }

    async fn start_with_notifications(notifications: bool) -> Self {
        use magician::magician_v2::agents::{
            AgentDefinitionStore, AgentMemoryResolver, AgentStorage,
        };
        use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
        use magician::magician_v2::execution::agent_resources::AgentResources;
        use magician::magician_v2::execution::capability::CapabilityRegistry;
        use magician::magician_v2::execution::compiled_providers::{
            default_compiled_handler_registry, embedded_compiled_pack_defs_ref,
            GenericCompiledProvider,
        };
        use magician::magician_v2::execution::flat_loop::build_tool_index;
        use std::sync::{OnceLock, RwLock};

        let dir = tempfile::tempdir().unwrap();
        let storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: dir.path().join("prompts"),
            ..Default::default()
        })
        .unwrap();
        let mut executors = ActionExecutors::new(
            Arc::new(NoLlm),
            Arc::new(PromptManager::new(Arc::new(storage))),
        );
        executors
            .file_sandbox
            .allowed_roots
            .push(dir.path().to_string_lossy().into_owned());
        let resources = Arc::new(AgentResources {
            magician_config: Arc::new(RwLock::new(Default::default())),
            memory_resolver: Arc::new(AgentMemoryResolver::new(dir.path())),
            agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(
                dir.path(),
            ))),
            artifact_workspace: ArtifactV2Workspace::new(dir.path()),
            artifact_v2_service: None,
            event_broadcaster: None,
            operation_llm_router: None,
            secret_store_resolver: None,
            content_acquisition_resolver: Arc::new(RwLock::new(None)),
            file_sandbox: executors.file_sandbox.clone(),
            tool_index: Arc::new(OnceLock::new()),
            user_request_service: None,
            agent_runtime: None,
        });
        // Use the production read handler and definition: approval tests must
        // reach a working provider, not merely increment the dispatch counter.
        let definition = embedded_compiled_pack_defs_ref()
            .iter()
            .find(|definition| definition.name == "read_file")
            .unwrap()
            .clone();
        let index = build_tool_index(std::slice::from_ref(&definition));
        let registry = Arc::new(CapabilityRegistry::new());
        registry.register(Arc::new(
            GenericCompiledProvider::new(
                "read_file",
                default_compiled_handler_registry()
                    .get("read_file")
                    .unwrap(),
                resources,
                None,
            )
            .with_pack_def(definition.clone()),
        ));
        registry.set_pack_definition("read_file", definition);
        executors.capability_registry = Some(registry);
        let mut ctx = AgenticContext::default();
        ctx.principal = Some("owner".into());
        ctx.workspace = Some("default".into());
        ctx.agent_id = Some("personal-assistant".into());
        let mut grant = PlaneGrant::for_terminal(
            ctx,
            uuid::Uuid::new_v4().to_string(),
            vec![],
            Arc::new(index),
        )
        .with_approval_rule_for("read_file");
        grant.executors = Some(Arc::new(executors));
        grant.catalog_profile = PlaneCatalogProfile::SpawnedBare;
        let token = plane_grant_registry().mint(grant.clone()).await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = HttpServer::new(move || {
            App::new().service(
                web::resource("/mcp")
                    .route(web::post().to(plane_mcp_handler))
                    .route(
                        web::get().to(move |request: actix_web::HttpRequest| async move {
                            if notifications {
                                plane_mcp_sse_handler(request, None).await
                            } else {
                                HttpResponse::MethodNotAllowed().finish()
                            }
                        }),
                    )
                    // A real MCP client closes with DELETE; the harness must
                    // answer it as the live door does, not 404.
                    .route(web::delete().to(plane_mcp_delete_handler)),
            )
        })
        .workers(1)
        .listen(listener)
        .unwrap()
        .run();
        let handle = server.handle();
        actix_web::rt::spawn(server);
        Self {
            url: format!("http://{addr}/mcp"),
            token,
            grant,
            server: handle,
            _dir: dir,
        }
    }

    async fn post(&self, session: Option<&str>, body: Value) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&body);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request.send().await.unwrap()
    }

    async fn initialize(&self, capabilities: Value) -> String {
        let response = self.post(None, json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":capabilities,"clientInfo":{"name":"contract","version":"1"}
        }})).await;
        assert_eq!(response.status(), 200);
        response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .into()
    }

    async fn call(&self, session: &str, id: i64, name: &str, arguments: Value) -> Sse {
        let response = self.post(Some(session), json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        Sse {
            response,
            buffered: String::new(),
        }
    }

    async fn answer(&self, session: &str, prompt: &Value, result: Value) {
        let response = self
            .post(
                Some(session),
                json!({"jsonrpc":"2.0","id":prompt["id"],"result":result}),
            )
            .await;
        assert_eq!(response.status(), 202);
        assert!(response.bytes().await.unwrap().is_empty());
    }

    async fn stop(self) {
        plane_grant_registry().revoke(&self.token).await;
        self.server.stop(false).await;
    }
}

struct Sse {
    response: reqwest::Response,
    buffered: String,
}
impl Sse {
    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(end) = self.buffered.find("\n\n") {
                    let frame = self.buffered[..end].to_string();
                    self.buffered.drain(..end + 2);
                    if let Some(data) = frame.lines().find_map(|l| l.strip_prefix("data: ")) {
                        return serde_json::from_str(data).unwrap();
                    }
                } else {
                    let chunk = self
                        .response
                        .chunk()
                        .await
                        .unwrap()
                        .expect("SSE ended before its result");
                    self.buffered.push_str(std::str::from_utf8(&chunk).unwrap());
                }
            }
        })
        .await
        .expect("SSE response deadline")
    }
}

#[actix_web::test]
async fn post_only_round_trips_text_choices_multi_choice_and_forms() {
    let door = Door::start().await;
    let session = door.initialize(json!({"elicitation":{"form":{}}})).await;
    let cases = [
        (
            json!({"question":"Name?","input_type":"text"}),
            json!({"value":"Ada"}),
            "text",
        ),
        (
            json!({"question":"Plan?","input_type":"choice","options":options()}),
            json!({"selected_id":"large"}),
            "choice",
        ),
        (
            json!({"question":"Plans?","input_type":"multi_choice","options":options()}),
            json!({"selected_ids":["small","large"]}),
            "multi_choice",
        ),
        (
            json!({"question":"Details?","input_type":"form","questions":[{"id":"name","prompt":"Name?"},{"id":"plan","prompt":"Plan?","input_type":"choice","options":options()}]}),
            json!({"name":"Ada","plan":"small"}),
            "form",
        ),
    ];
    for (index, (args, content, kind)) in cases.into_iter().enumerate() {
        let mut stream = door
            .call(&session, index as i64 + 1, "request_user_input", args)
            .await;
        let prompt = stream.next().await;
        assert_eq!(prompt["method"], "elicitation/create");
        assert_eq!(prompt["params"]["mode"], "form");
        door.answer(
            &session,
            &prompt,
            json!({"action":"accept","content":content}),
        )
        .await;
        let result = stream.next().await;
        assert_eq!(result["id"], index as i64 + 1);
        assert_eq!(result["result"]["isError"], false);
        assert_eq!(
            result["result"]["structuredContent"]["answer"]["type"],
            kind
        );
        assert!(
            stream.response.chunk().await.unwrap().is_none(),
            "exactly one final result"
        );
    }
    door.stop().await;
}

#[actix_web::test]
async fn independent_clients_reusing_initialize_id_cannot_answer_each_other() {
    let door = Door::start().await;
    let a = door.initialize(json!({"elicitation":{}})).await;
    let b = door.initialize(json!({"elicitation":{}})).await;
    assert_ne!(a, b);
    let mut sa = door
        .call(&a, 1, "request_user_input", json!({"question":"A?"}))
        .await;
    let mut sb = door
        .call(&b, 1, "request_user_input", json!({"question":"B?"}))
        .await;
    let pa = sa.next().await;
    let pb = sb.next().await;
    assert_ne!(pa["id"], pb["id"]);
    let foreign = door.post(Some(&b), json!({"jsonrpc":"2.0","id":pa["id"],"result":{"action":"accept","content":{"value":"wrong"}}})).await;
    assert_eq!(foreign.status(), 400);
    door.answer(&a, &pa, json!({"action":"accept","content":{"value":"A"}}))
        .await;
    door.answer(&b, &pb, json!({"action":"accept","content":{"value":"B"}}))
        .await;
    assert_eq!(
        sa.next().await["result"]["structuredContent"]["answer"]["value"],
        "A"
    );
    assert_eq!(
        sb.next().await["result"]["structuredContent"]["answer"]["value"],
        "B"
    );
    door.stop().await;
}

#[actix_web::test]
async fn false_malformed_declined_and_cancelled_approvals_never_dispatch() {
    let door = Door::start().await;
    let session = door.initialize(json!({"elicitation":{}})).await;
    for (index, answer) in [
        json!({"action":"accept","content":{"confirm":false}}),
        json!({"action":"accept"}),
        json!({"action":"accept","content":{"confirm":"true"}}),
        json!({"action":"decline"}),
        json!({"action":"cancel"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut stream = door
            .call(
                &session,
                index as i64 + 1,
                "read_file",
                json!({"file_path":"Cargo.toml"}),
            )
            .await;
        let prompt = stream.next().await;
        assert_eq!(prompt["method"], "elicitation/create");
        assert!(prompt.to_string().find("planeApprovalCapture").is_none());
        door.answer(&session, &prompt, answer).await;
        assert_eq!(stream.next().await["result"]["isError"], true);
        assert_eq!(door.grant.turn_tool_calls_spent(), 0);
    }
    door.stop().await;
}

#[actix_web::test]
async fn two_approval_captures_execute_independently_and_replay_does_not_execute_again() {
    let door = Door::start().await;
    let first_path = door._dir.path().join("first.txt");
    let second_path = door._dir.path().join("second.txt");
    std::fs::write(&first_path, "first-captured-action").unwrap();
    std::fs::write(&second_path, "second-captured-action").unwrap();
    let session = door.initialize(json!({"elicitation":{}})).await;
    let mut a = door
        .call(&session, 1, "read_file", json!({"file_path":first_path}))
        .await;
    let mut b = door
        .call(&session, 2, "read_file", json!({"file_path":second_path}))
        .await;
    let pa = a.next().await;
    let pb = b.next().await;
    door.answer(
        &session,
        &pa,
        json!({"action":"accept","content":{"confirm":true}}),
    )
    .await;
    let first = a.next().await;
    assert_eq!(first["result"]["isError"], false, "{first}");
    assert!(first.to_string().contains("first-captured-action"));
    assert!(!first.to_string().contains("second-captured-action"));
    assert_eq!(door.grant.turn_tool_calls_spent(), 1);
    door.answer(
        &session,
        &pb,
        json!({"action":"accept","content":{"confirm":true}}),
    )
    .await;
    let second = b.next().await;
    assert_eq!(second["result"]["isError"], false, "{second}");
    assert!(second.to_string().contains("second-captured-action"));
    assert!(!second.to_string().contains("first-captured-action"));
    assert_eq!(door.grant.turn_tool_calls_spent(), 2);
    let duplicate = door.post(Some(&session), json!({"jsonrpc":"2.0","id":pa["id"],"result":{"action":"accept","content":{"confirm":true}}})).await;
    assert_eq!(duplicate.status(), 400);
    let replay = door.post(Some(&session), json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_file","arguments":{"file_path":first_path}}})).await;
    assert_eq!(replay.json::<Value>().await.unwrap(), first);
    assert_eq!(door.grant.turn_tool_calls_spent(), 2);
    door.stop().await;
}

#[actix_web::test]
async fn cancellation_and_revocation_end_only_the_bound_pending_call() {
    let door = Door::start().await;
    let session = door.initialize(json!({"elicitation":{}})).await;
    let mut a = door
        .call(&session, 1, "request_user_input", json!({"question":"A?"}))
        .await;
    let mut b = door
        .call(&session, 2, "request_user_input", json!({"question":"B?"}))
        .await;
    let pa = a.next().await;
    let pb = b.next().await;
    let cancelled = door
        .post(
            Some(&session),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        )
        .await;
    assert_eq!(cancelled.status(), 202);
    assert_eq!(a.next().await["result"]["isError"], true);
    door.answer(
        &session,
        &pb,
        json!({"action":"accept","content":{"value":"B"}}),
    )
    .await;
    assert_eq!(b.next().await["result"]["isError"], false);
    let stale = door.post(Some(&session), json!({"jsonrpc":"2.0","id":pa["id"],"result":{"action":"accept","content":{"value":"A"}}})).await;
    assert_eq!(stale.status(), 400);
    let mut c = door
        .call(&session, 3, "read_file", json!({"file_path":"Cargo.toml"}))
        .await;
    c.next().await;
    plane_grant_registry().revoke(&door.token).await;
    assert_eq!(c.next().await["result"]["isError"], true);
    assert_eq!(door.grant.turn_tool_calls_spent(), 0);
    door.stop().await;
}

#[actix_web::test]
async fn headless_url_only_and_foreign_run_calls_fail_closed() {
    let door = Door::start().await;
    for capabilities in [
        json!({}),
        json!({"elicitation":{"url":{}}}),
        json!({"elicitation":null}),
    ] {
        let session = door.initialize(capabilities).await;
        let response = door.post(Some(&session), json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_file","arguments":{"file_path":"Cargo.toml"}}})).await;
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json"));
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["result"]["isError"], true);
        assert!(result["result"].get("_meta").is_none());
    }
    let session = door.initialize(json!({"elicitation":{}})).await;
    let mut stream = door
        .call(
            &session,
            1,
            "wait_for_run",
            json!({"execution_id":"somebody-elses-execution"}),
        )
        .await;
    assert_eq!(stream.next().await["result"]["isError"], true);
    door.stop().await;
}

struct SdkClient {
    prompts: Arc<AtomicUsize>,
}
impl rmcp::ClientHandler for SdkClient {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        serde_json::from_value(json!({"protocolVersion":"2025-11-25","capabilities":{"elicitation":{"form":{}}},"clientInfo":{"name":"plane-contract","version":"1"}})).unwrap()
    }
    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        assert!(matches!(
            request,
            rmcp::model::ElicitRequestParams::FormElicitationParams { .. }
        ));
        let schema = serde_json::to_value(&request).unwrap();
        let properties = &schema["requestedSchema"]["properties"];
        let content = if properties.get("selected_id").is_some() {
            json!({"selected_id":"large"})
        } else if properties.get("selected_ids").is_some() {
            json!({"selected_ids":["small","large"]})
        } else if properties.get("name").is_some() {
            json!({"name":"SDK answer","plan":"small"})
        } else {
            json!({"value":"SDK answer"})
        };
        self.prompts.fetch_add(1, Ordering::SeqCst);
        Ok(
            rmcp::model::ElicitResult::new(rmcp::model::ElicitationAction::Accept)
                .with_content(content),
        )
    }
}

#[actix_web::test]
async fn official_sdk_completes_elicitation_with_the_standard_wire_contract() {
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    };
    use rmcp::ServiceExt;
    let door = Door::start_with_notifications(false).await;
    let prompts = Arc::new(AtomicUsize::new(0));
    let transport = StreamableHttpClientTransport::with_client(
        reqwest13::Client::new(),
        StreamableHttpClientTransportConfig::with_uri(door.url.clone())
            .auth_header(door.token.clone()),
    );
    let client = SdkClient {
        prompts: prompts.clone(),
    }
    .serve(transport)
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(
            rmcp::model::CallToolRequestParams::new("request_user_input")
                .with_arguments(json!({"question":"Name?"}).as_object().unwrap().clone()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.is_error, Some(false));
    assert_eq!(prompts.load(Ordering::SeqCst), 1);
    assert_eq!(
        result.structured_content.unwrap()["answer"]["value"],
        "SDK answer"
    );
    for (arguments, kind) in [
        (
            json!({"question":"Plan?","input_type":"choice","options":options()}),
            "choice",
        ),
        (
            json!({"question":"Plans?","input_type":"multi_choice","options":options()}),
            "multi_choice",
        ),
        (
            json!({"question":"Details?","input_type":"form","questions":[{"id":"name","prompt":"Name?"},{"id":"plan","prompt":"Plan?","input_type":"choice","options":options()}]}),
            "form",
        ),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.call_tool(
                rmcp::model::CallToolRequestParams::new("request_user_input")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(result.structured_content.unwrap()["answer"]["type"], kind);
    }
    assert_eq!(prompts.load(Ordering::SeqCst), 4);
    client.cancel().await.unwrap();
    door.stop().await;
}

#[test]
fn capabilities_use_the_standard_field_and_negotiate_each_mode() {
    use elicitation::ClientCapabilities;
    for declaration in [json!({}), json!({"form":{}}), json!({"form":{},"url":{}})] {
        assert!(ClientCapabilities::from_initialize(
            &json!({"params":{"capabilities":{"elicitation":declaration}}})
        )
        .supports("form"));
    }
    let url = ClientCapabilities::from_initialize(
        &json!({"params":{"capabilities":{"elicitation":{"url":{}}}}}),
    );
    assert!(url.supports("url"));
    assert!(!url.supports("form"));
    assert!(!ClientCapabilities::from_initialize(
        &json!({"params":{"clientCapabilities":{"elicitation":{}}}})
    )
    .supports("form"));
    assert!(!ClientCapabilities::from_initialize(
        &json!({"params":{"capabilities":{"elicitation":null}}})
    )
    .supports("form"));
    assert!(!ClientCapabilities::from_initialize(
        &json!({"params":{"capabilities":{"elicitation":{"form":false}}}})
    )
    .supports("form"));
}

#[actix_web::test]
async fn optional_get_is_session_scoped_and_never_receives_another_calls_prompt() {
    let door = Door::start().await;
    let a = door.initialize(json!({"elicitation":{}})).await;
    let b = door.initialize(json!({"elicitation":{}})).await;
    let response = reqwest::Client::new()
        .get(&door.url)
        .bearer_auth(&door.token)
        .header("mcp-session-id", &b)
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut background = Sse {
        response,
        buffered: String::new(),
    };
    let mut stream = door
        .call(
            &a,
            1,
            "request_user_input",
            json!({"question":"Private question?"}),
        )
        .await;
    let prompt = stream.next().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), background.next())
            .await
            .is_err()
    );
    door.answer(&a, &prompt, json!({"action":"cancel"})).await;
    stream.next().await;
    drop(background);
    door.stop().await;
}

struct RoutedChannel(Arc<elicitation::CallState>);
#[async_trait::async_trait]
impl magician::magician_v2::execution::plane::input::InputChannel for RoutedChannel {
    async fn ask(
        &self,
        question: &str,
        input_type: UserInputType,
    ) -> Result<UserInputValue, String> {
        self.0.ask(question, input_type).await
    }
}

#[tokio::test]
async fn service_input_uses_the_existing_response_owner_and_wakes_the_caller() {
    use magician::magician_v2::execution::plane::input::{InputRoute, INPUT_ROUTE};
    use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
    use magician::magician_v2::user_requests::{UserRequest, UserRequestService};
    let service = UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(32)));
    let (call, mut messages) =
        elicitation::CallState::new("owner-session".into(), json!(1), "runtime_tool".into());
    let route = InputRoute {
        principal: "owner".into(),
        workspace: "default".into(),
        channel: Arc::new(RoutedChannel(call.clone())),
    };
    let request = UserRequest {
        id: String::new(),
        request_type: "need_user_input".into(),
        question: "Which plan?".into(),
        options: vec![],
        principal: "owner".into(),
        workspace: "default".into(),
        context: json!({"input_type":"choice", "input_schema":{"input_type":"choice","options":options()}}),
        source: "chat".into(),
        execution_id: Some("execution-1".into()),
        task_id: None,
        timeout_secs: 30,
        default_on_timeout: "timeout".into(),
        created_at: 0,
        sensitive: None,
    };
    let mut malformed = request.clone();
    malformed.context = json!({"input_schema": ["invalid"]});
    assert!(
        magician::magician_v2::execution::plane::input::service_input_type(&malformed).is_err()
    );
    let response = INPUT_ROUTE.scope(route, service.ask(request));
    tokio::pin!(response);
    let message = tokio::select! { message = messages.recv() => message.unwrap(), _ = &mut response => panic!("service must await the answer") };
    let prompt: Value = serde_json::from_str(
        message
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .unwrap(),
    )
    .unwrap();
    assert!(call.answer("owner-session", &json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"action":"accept","content":{"selected_id":"large"}}})));
    let response = response.await;
    assert_eq!(response.decision, "large");
    assert_eq!(response.channel, "mcp");
    assert!(service
        .list_pending_for_scope("owner", "default")
        .await
        .is_empty());
    assert!(
        !service.respond(response).await,
        "service response is still first-response-wins"
    );
}
