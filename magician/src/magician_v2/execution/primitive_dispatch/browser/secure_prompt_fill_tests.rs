use super::*;
use crate::magician_v2::{
    realtime_events::RuntimeTransportBroadcaster,
    user_requests::{ScopedResponseResult, UserResponse},
};
use async_trait::async_trait;
use magicvault_effect::{Outcome, Target};
use magicvault_protocol::ErrorCode;
use std::sync::atomic::{AtomicUsize, Ordering};
const CANARY: &str = "jit-browser-secret-canary-13";
struct Adapter {
    targets: Vec<Target>,
    calls: AtomicUsize,
    stale: bool,
    /// What each fill delivered, so a test can assert on the values without
    /// the adapter knowing which test it serves.
    delivered: Mutex<Vec<Vec<String>>>,
    /// After this many target reads the page shows another document (a
    /// navigation between the prompt and the fill); 0 = never.
    rotate_document_after: AtomicUsize,
    target_reads: AtomicUsize,
}
#[async_trait]
impl BrowserAdapter for Adapter {
    async fn targets(&self, _: CancellationToken) -> Result<Vec<Target>, ErrorCode> {
        let reads = self.target_reads.fetch_add(1, Ordering::SeqCst) + 1;
        let rotate_after = self.rotate_document_after.load(Ordering::SeqCst);
        let mut targets = self.targets.clone();
        if rotate_after > 0 && reads > rotate_after {
            for target in targets.iter_mut() {
                target.document = "rotated-document".into();
                target.top_document = "rotated-document".into();
            }
        }
        Ok(targets)
    }
    async fn fill(
        &self,
        target: &Target,
        fields: Vec<MaterialField>,
        _: CancellationToken,
    ) -> Outcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(target.document, "original-document");
        self.delivered
            .lock()
            .unwrap()
            .push(fields.iter().map(|f| f.value.clone()).collect());
        if self.stale {
            Outcome::failed(fields.len(), ErrorCode::StaleTarget)
        } else {
            Outcome {
                fields: vec![FieldState::Filled; fields.len()],
                error: None,
            }
        }
    }
    fn disconnect(&self) {}
    fn connected(&self) -> bool {
        true
    }
}
fn args() -> Arguments {
    serde_json::from_value(json!({"top_origin":"https://example.com","fields":[{"field_name":"username","css":"#username"},{"field_name":"password","css":"#password"}]})).unwrap()
}
fn adapter(stale: bool) -> Arc<Adapter> {
    Arc::new(Adapter {
        targets: vec![Target {
            tab: "tab1".into(),
            frame: "frame1".into(),
            document: "original-document".into(),
            top_document: "original-document".into(),
            origin: "https://example.com".into(),
            top_origin: "https://example.com".into(),
            is_main_frame: true,
        }],
        calls: AtomicUsize::new(0),
        stale,
        delivered: Mutex::new(Vec::new()),
        rotate_document_after: AtomicUsize::new(0),
        target_reads: AtomicUsize::new(0),
    })
}
fn no_material() -> (Option<Arc<SecretStore>>, Option<String>) {
    (None, None)
}
fn svc() -> Arc<UserRequestService> {
    Arc::new(UserRequestService::new(Arc::new(
        RuntimeTransportBroadcaster::new(64),
    )))
}
fn start(
    service: Arc<UserRequestService>,
    adapter: Arc<Adapter>,
) -> tokio::task::JoinHandle<Value> {
    start_with(service, adapter, args(), no_material(), None, None)
}
fn start_with(
    service: Arc<UserRequestService>,
    adapter: Arc<Adapter>,
    args: Arguments,
    (store, scope): (Option<Arc<SecretStore>>, Option<String>),
    delivered: Option<Arc<Mutex<KnownSecretValues>>>,
    capture_withheld: Option<Arc<AtomicU8>>,
) -> tokio::task::JoinHandle<Value> {
    tokio::spawn(async move {
        let prompt = PromptContext {
            service: &service,
            principal: "owner",
            workspace: "workspace",
            execution_id: Some("exec".into()),
            task_id: None,
            owner_agent_id: None,
            chat_session_id: None,
            deadline: tokio::time::Instant::now() + Duration::from_secs(DEADLINE_SECS),
        };
        let material = MaterialContext {
            store: store.as_deref(),
            scope: scope.as_deref(),
            delivered: delivered.as_ref(),
            capture_withheld: capture_withheld.as_ref(),
            pending_challenge: None,
        };
        run(
            adapter.as_ref(),
            &args,
            &prompt,
            &material,
            CancellationToken::new(),
        )
        .await
    })
}
fn store_with_material() -> (Arc<SecretStore>, String) {
    let dir = std::env::temp_dir().join(format!("secure-fill-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(SecretStore::new_empty(
        Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
        dir,
    ));
    let scope = "execution:exec".to_string();
    store
        .register_ephemeral_bounded(
            &scope,
            "password",
            CANARY.to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .unwrap();
    store
        .register_one_time(
            &scope,
            "otp",
            "042917".to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
            crate::magician_v2::secrets::OneTimeBinding {
                challenge_id: Some("challenge-1".into()),
                destination: Some("https://example.com".into()),
            },
        )
        .unwrap();
    // The run records the challenge's destination beside the material, as
    // `vault_flagged_resolved_inputs_bound` does.
    store
        .register_ephemeral_bounded(
            &scope,
            &crate::magician_v2::secrets::sinks::binding_key("otp"),
            "https://example.com".to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .unwrap();
    (store, scope)
}
fn referenced_args(fields: Value) -> Arguments {
    serde_json::from_value(json!({"top_origin":"https://example.com","fields":fields})).unwrap()
}
async fn answer_next(
    service: &UserRequestService,
    decision: &str,
    input: Option<&str>,
    request_type: &str,
) {
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(r) = service.list_pending().await.into_iter().next() {
                return r;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(request.request_type, request_type);
    assert!(!serde_json::to_string(&request).unwrap().contains(CANARY));
    if request_type == "secure_browser_input" {
        assert_eq!(request.context["input_type"], "password");
    }
    assert_eq!(
        service
            .respond_scoped(
                UserResponse {
                    request_id: request.id,
                    decision: decision.into(),
                    input: input.map(str::to_string),
                    channel: "web".into(),
                    sensitive: Vec::new(),
                },
                Some("owner"),
                Some("workspace")
            )
            .await,
        ScopedResponseResult::Accepted
    );
}
async fn provide_fields(service: &UserRequestService) {
    for _ in 0..2 {
        answer_next(
            service,
            "provide_input",
            Some(CANARY),
            "secure_browser_input",
        )
        .await;
    }
}
#[tokio::test]
async fn jit_browser_prompts_all_fields_then_confirms_and_returns_only_receipt() {
    let service = svc();
    let adapter = adapter(false);
    let task = start(service.clone(), adapter.clone());
    provide_fields(&service).await;
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    answer_next(&service, "allow_once", None, "secure_browser_confirm").await;
    let result = task.await.unwrap();
    assert_eq!(result["status"], "filled");
    assert_eq!(result["saved"], false);
    assert_eq!(result["submitted"], false);
    assert!(!result.to_string().contains(CANARY));
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *adapter.delivered.lock().unwrap(),
        vec![vec![CANARY.to_string(), CANARY.to_string()]]
    );
    assert!(
        !serde_json::to_string(&service.list_history_for_scope("owner", "workspace", None))
            .unwrap()
            .contains(CANARY)
    );
}
#[tokio::test]
async fn jit_browser_decline_discards_values_without_delivery() {
    let service = svc();
    let adapter = adapter(false);
    let task = start(service.clone(), adapter.clone());
    provide_fields(&service).await;
    answer_next(&service, "cancel", Some(CANARY), "secure_browser_confirm").await;
    assert_eq!(task.await.unwrap()["status"], "cancelled");
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert!(
        !serde_json::to_string(&service.list_history_for_scope("owner", "workspace", None))
            .unwrap()
            .contains(CANARY)
    );
}
#[tokio::test]
async fn jit_browser_stale_target_is_not_rediscovered_or_retried() {
    let service = svc();
    let adapter = adapter(true);
    let task = start(service.clone(), adapter.clone());
    provide_fields(&service).await;
    answer_next(&service, "allow_once", None, "secure_browser_confirm").await;
    let result = task.await.unwrap();
    assert_eq!(result["error"], "stale_target");
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn jit_browser_ambiguous_target_refuses_before_prompt() {
    let service = svc();
    let mut adapter = adapter(false);
    let inner = Arc::get_mut(&mut adapter).unwrap();
    inner.targets.push(inner.targets[0].clone());
    assert_eq!(
        start(service.clone(), adapter.clone()).await.unwrap()["status"],
        "ambiguous_target"
    );
    assert!(service.list_pending().await.is_empty());
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
}
// ---- P4 Task 4.4: referenced material through the typed fill --------------
#[tokio::test]
async fn a_bound_code_fills_once_without_a_second_confirmation_and_is_never_filled_again() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    let delivered = Arc::new(Mutex::new(KnownSecretValues::new()));
    let withheld = Arc::new(AtomicU8::new(0));
    let fields = json!([{"field_name":"code","css":"#otp","value":"[REF:otp]"}]);
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(fields.clone()),
        (Some(store.clone()), Some(scope.clone())),
        Some(delivered.clone()),
        Some(withheld.clone()),
    )
    .await
    .unwrap();
    // No prompt at all: the code was collected for exactly this origin.
    assert_eq!(result["status"], "filled", "{result}");
    assert!(service.list_pending().await.is_empty());
    assert_eq!(
        *adapter.delivered.lock().unwrap(),
        vec![vec!["042917".to_string()]]
    );
    assert_eq!(
        store.one_time_state(&scope, "otp").map(|s| s.state),
        Some(crate::magician_v2::secrets::OneTimeState::Consumed)
    );
    assert!(
        delivered
            .lock()
            .unwrap()
            .values()
            .any(|value| value == "042917"),
        "the run scrubs it from now on"
    );
    assert_eq!(
        withheld.load(Ordering::SeqCst),
        CAPTURE_WITHHELD_PIXELS,
        "pixels are withheld until the page moves on"
    );
    assert!(!result.to_string().contains("042917"));

    // The same reference again: refused before any prompt or fill.
    let again = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(fields),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(again["status"], "material_unavailable", "{again}");
    assert!(again["reason"].as_str().unwrap().contains("consumed"));
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn a_referenced_password_still_asks_for_use_once_and_a_decline_releases_a_code() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    let task = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(json!([
            {"field_name":"password","css":"#password","value":"[REF:password]"},
            {"field_name":"code","css":"#otp","value":"[REF:otp]"}
        ])),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    );
    // A password has no destination binding, so the user confirms — and the
    // code is only reserved, not spent, while they decide.
    answer_next(&service, "cancel", None, "secure_browser_confirm").await;
    assert_eq!(task.await.unwrap()["status"], "cancelled");
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.one_time_state(&scope, "otp").map(|s| s.state),
        Some(crate::magician_v2::secrets::OneTimeState::Available),
        "a decline is a proven pre-dispatch failure: the code is released"
    );
    // Confirmed, both deliver — the password from the scope, the code once.
    let task = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(json!([
            {"field_name":"password","css":"#password","value":"[REF:password]"},
            {"field_name":"code","css":"#otp","value":"[REF:otp]"}
        ])),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    );
    answer_next(&service, "allow_once", None, "secure_browser_confirm").await;
    assert_eq!(task.await.unwrap()["status"], "filled");
    assert_eq!(
        *adapter.delivered.lock().unwrap(),
        vec![vec![CANARY.to_string(), "042917".to_string()]]
    );
}
#[tokio::test]
async fn a_password_bound_to_this_origin_still_asks_for_use_once() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    // The run recorded a destination beside the password too — a challenge
    // binds every kind. That binding says where the value may go; it is not
    // the user's consent to spend it, because a password is not spent: it
    // outlives the fill under the run's scope, so the ask stands.
    store
        .register_ephemeral_bounded(
            &scope,
            &crate::magician_v2::secrets::sinks::binding_key("password"),
            "https://example.com".to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .unwrap();
    let task = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(
            json!([{"field_name":"password","css":"#password","value":"[REF:password]"}]),
        ),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    );
    answer_next(&service, "allow_once", None, "secure_browser_confirm").await;
    assert_eq!(task.await.unwrap()["status"], "filled");
    assert_eq!(
        *adapter.delivered.lock().unwrap(),
        vec![vec![CANARY.to_string()]]
    );
}
#[tokio::test]
async fn a_reference_to_another_origin_or_a_typed_value_is_refused() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    // Bound to https://example.com; the target here is the same origin, so
    // bind the code elsewhere to prove the claim check.
    store
        .register_one_time(
            &scope,
            "otp",
            "999999".to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
            crate::magician_v2::secrets::OneTimeBinding {
                challenge_id: None,
                destination: Some("https://other.example".into()),
            },
        )
        .unwrap();
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(json!([{"field_name":"code","css":"#otp","value":"[REF:otp]"}])),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "material_unavailable", "{result}");
    assert!(
        result["reason"].as_str().unwrap().contains("binding"),
        "{result}"
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert!(!result.to_string().contains("999999"));
    // A typed value is never accepted as a field value.
    let typed: Arguments = serde_json::from_value(json!({"top_origin":"https://example.com","fields":[{"field_name":"code","css":"#otp","value":"042917"}]})).unwrap();
    assert!(!typed.valid());
    let two_refs: Arguments = serde_json::from_value(json!({"top_origin":"https://example.com","fields":[{"field_name":"code","css":"#otp","value":"[REF:otp] [REF:password]"}]})).unwrap();
    assert!(!two_refs.valid());
}
#[tokio::test]
async fn a_password_bound_to_another_origin_is_refused_before_any_prompt() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    store
        .register_ephemeral_bounded(
            &scope,
            &crate::magician_v2::secrets::sinks::binding_key("password"),
            "https://other.example".to_string(),
            chrono::Utc::now().timestamp_millis() + 60_000,
        )
        .unwrap();
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(
            json!([{"field_name":"password","css":"#password","value":"[REF:password]"}]),
        ),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "material_unavailable", "{result}");
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("bound to https://other.example"),
        "{result}"
    );
    assert!(service.list_pending().await.is_empty());
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
    assert!(!result.to_string().contains(CANARY));
}
#[tokio::test]
async fn a_page_that_changed_before_the_fill_releases_the_code() {
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    // The first target read (before the prompts) sees the page; the second
    // (immediately before the codes are spent) sees a different document.
    adapter.rotate_document_after.store(1, Ordering::SeqCst);
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(json!([{"field_name":"code","css":"#otp","value":"[REF:otp]"}])),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "target_unavailable", "{result}");
    assert_eq!(
        adapter.calls.load(Ordering::SeqCst),
        0,
        "nothing was filled"
    );
    assert_eq!(
        store.one_time_state(&scope, "otp").map(|s| s.state),
        Some(crate::magician_v2::secrets::OneTimeState::Available),
        "the code was reserved and released, never spent"
    );
}
#[test]
fn jit_browser_validates_metadata_and_runtime_replay_identity() {
    assert_eq!(log::STATIC_MAX_LEVEL, log::LevelFilter::Off);
    assert!(args().valid());
    let mut bad = args();
    bad.top_origin = "http://example.com".into();
    assert!(!bad.valid());
    let mut bad = args();
    bad.fields[1].css = bad.fields[0].css.clone();
    assert!(!bad.valid());
    assert!(serde_json::from_value::<Arguments>(
        json!({"top_origin":"https://example.com","fields":[],"password":CANARY})
    )
    .is_err());
    let id = uuid::Uuid::new_v4().to_string();
    assert!(claim("owner", "workspace", &id));
    assert!(!claim("owner", "workspace", &id));
    assert!(claim("owner2", "workspace", &id));
}
#[test]
fn jit_browser_skill_contract_parses_and_advertises_metadata_only() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("skillshub/browser/SKILL.md");
    let source = std::fs::read_to_string(path).unwrap();
    assert!(
        tool_runtime_core::manifest_parser::parse_skill_runtime_package(&source)
            .unwrap()
            .is_some()
    );
    let frontmatter = source.split("---").nth(1).unwrap();
    let value: serde_json::Value = serde_yaml::from_str(frontmatter).unwrap();
    let action = &value["metadata"]["magician"]["runtime_actions"]["actions"]["secure_prompt_fill"];
    assert!(action["parameters"]["fields"]["required"]
        .as_bool()
        .unwrap());
    assert!(action["parameters"].get("password").is_none());
}
#[tokio::test]
async fn a_code_split_across_digit_fields_is_one_reserve_one_fill_and_withholds_every_observation()
{
    let service = svc();
    let adapter = adapter(false);
    let (store, scope) = store_with_material();
    let delivered = Arc::new(Mutex::new(KnownSecretValues::new()));
    let withheld = Arc::new(AtomicU8::new(0));
    let six = |n: usize| -> Value {
        Value::Array(
            (1..=n)
                .map(|i| json!({"field_name": format!("digit{i}"), "css": format!("#d{i}"), "value": "[REF:otp]"}))
                .collect(),
        )
    };
    // Six boxes for a six-character code: resolved once, split in order,
    // filled in one operation, the code spent once.
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(six(6)),
        (Some(store.clone()), Some(scope.clone())),
        Some(delivered.clone()),
        Some(withheld.clone()),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "filled", "{result}");
    assert!(
        service.list_pending().await.is_empty(),
        "bound to this origin: no confirmation"
    );
    assert_eq!(
        *adapter.delivered.lock().unwrap(),
        vec![["0", "4", "2", "9", "1", "7"].map(String::from).to_vec()]
    );
    assert_eq!(
        store.one_time_state(&scope, "otp").map(|s| s.state),
        Some(crate::magician_v2::secrets::OneTimeState::Consumed)
    );
    assert!(
        delivered
            .lock()
            .unwrap()
            .values()
            .any(|value| value == "042917"),
        "the whole code is scrubbed"
    );
    assert_eq!(
        withheld.load(Ordering::SeqCst),
        CAPTURE_WITHHELD_PAGE,
        "a split value is recognisable in no text: every observation waits for a navigation"
    );
    assert!(!result.to_string().contains("042917"));

    // A count that does not match the material refuses before anything is
    // asked, filled or spent — and the reservation goes back.
    let (store, scope) = store_with_material();
    let result = start_with(
        service.clone(),
        adapter.clone(),
        referenced_args(six(4)),
        (Some(store.clone()), Some(scope.clone())),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "invalid_request", "{result}");
    assert!(result["reason"].as_str().unwrap().contains("4 fields"));
    assert!(
        !result.to_string().contains("042917"),
        "the reason names the count, never the code"
    );
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1, "no second fill");
    assert_eq!(
        store.one_time_state(&scope, "otp").map(|s| s.state),
        Some(crate::magician_v2::secrets::OneTimeState::Available),
        "the reservation was released"
    );
}
