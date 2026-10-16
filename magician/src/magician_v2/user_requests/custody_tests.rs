//! P1 Task 1.3 — custody before resolution. A sensitive answer enters the
//! service's custody before history, pending persistence, lifecycle events, or
//! the ordinary oneshot see it, and leaves only through one in-process take.
use super::*;
use serde_json::json;
use std::sync::Arc;

fn service() -> UserRequestService {
    UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
}

fn chat_request(question: &str, context: serde_json::Value) -> UserRequest {
    UserRequest {
        id: String::new(),
        request_type: "need_user_input".into(),
        question: question.into(),
        options: vec![],
        principal: "owner".into(),
        workspace: "workspace".into(),
        context,
        source: "chat".into(),
        execution_id: Some("chat-session-1".into()),
        task_id: None,
        timeout_secs: 60,
        default_on_timeout: "timeout".into(),
        created_at: 0,
        sensitive: None,
    }
}

fn password_request() -> UserRequest {
    chat_request(
        "Sign in",
        json!({ "input_type": "password", "chat_session_id": "chat-session-1" }),
    )
}

async fn pending(svc: &UserRequestService) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(r) = svc.list_pending().await.first() {
                return r.id.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

fn provide(id: String, input: &str) -> UserResponse {
    UserResponse {
        request_id: id,
        decision: "provide_input".into(),
        input: Some(input.into()),
        channel: "web".into(),
        sensitive: Vec::new(),
    }
}

fn assert_no_canary(svc: &UserRequestService, canary: &str) {
    let history = svc.list_history_for_scope("owner", "workspace", None);
    let rendered = serde_json::to_string(&history).unwrap();
    assert!(
        !rendered.contains(canary),
        "history carried the value: {rendered}"
    );
}

/// Simulate the crash of a persisted service: abort its timeout owners and
/// wait for its detached publication worker to finish. That worker holds a
/// clone of the durable store writer lease, and a successor service that
/// opens the same paths while it is alive is fenced from recovery rather
/// than handed the shard.
async fn stop_in_process_owners(svc: &UserRequestService) {
    let handles: Vec<_> = svc
        .pending
        .write()
        .await
        .by_id
        .values_mut()
        .filter_map(|entry| entry.timeout_handle.take())
        .collect();
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while svc
            .generic_request_publication_retry_started
            .load(std::sync::atomic::Ordering::Acquire)
            || svc
                .generic_resolution_publication_retry_started
                .load(std::sync::atomic::Ordering::Acquire)
        {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the in-process publication owners must stop before a restart");
}

/// Receive events until `done` matches one (that event included) or two
/// seconds pass. Publication can trail the resolution the asker awaited, so
/// a `try_recv` drain right after the round trip races the emitter.
async fn events_until(
    events: &mut tokio::sync::broadcast::Receiver<RuntimeTransportEvent>,
    done: impl Fn(&RuntimeTransportEvent) -> bool,
) -> Vec<RuntimeTransportEvent> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(event)) => {
                let finished = done(&event);
                seen.push(event);
                if finished {
                    return seen;
                }
            },
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => return seen,
        }
    }
}

#[tokio::test]
async fn a_chat_password_answer_is_taken_once_from_custody() {
    const CANARY: &str = "custody-canary-1";
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    let request_id = id.clone();
    assert_eq!(
        svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
            .await,
        ScopedResponseResult::Accepted
    );
    let response = asked.await.unwrap();

    assert_eq!(
        response.input, None,
        "the ordinary response must be value-free"
    );
    assert_eq!(response.channel, "secure_ui");
    assert_eq!(response.sensitive.len(), 1);
    let answer = &response.sensitive[0];
    assert_eq!(answer.kind, SensitiveKind::Password);
    assert_eq!(answer.status, SensitiveAnswerStatus::Provided);
    assert_eq!(answer.field, None);
    assert!(answer.reference.starts_with("sr_"));

    assert_eq!(
        svc.take_sensitive(&answer.reference, &request_id, "owner", "workspace")
            .as_deref()
            .map(|v| v.as_str()),
        Some(CANARY)
    );
    assert!(
        svc.take_sensitive(&answer.reference, &request_id, "owner", "workspace")
            .is_none(),
        "a second take must find nothing"
    );
    assert_no_canary(&svc, CANARY);
}

#[tokio::test]
async fn custody_refuses_a_scope_mismatch_without_consuming() {
    const CANARY: &str = "custody-canary-2";
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    let request_id = id.clone();
    svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
        .await;
    let reference = asked.await.unwrap().sensitive[0].reference.clone();

    assert!(
        svc.take_sensitive(&reference, "some-other-request", "owner", "workspace")
            .is_none(),
        "a reference is bound to its request"
    );
    assert!(svc
        .take_sensitive(&reference, &request_id, "intruder", "workspace")
        .is_none());
    assert!(svc
        .take_sensitive(&reference, &request_id, "owner", "elsewhere")
        .is_none());
    assert_eq!(
        svc.take_sensitive(&reference, &request_id, "owner", "workspace")
            .as_deref()
            .map(|v| v.as_str()),
        Some(CANARY),
        "a refused take must not consume the material"
    );
}

#[tokio::test]
async fn custody_expires_with_the_collection_deadline() {
    const CANARY: &str = "custody-canary-3";
    let svc = Arc::new(service());
    let mut request = password_request();
    request.timeout_secs = 1;
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    let request_id = id.clone();
    svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
        .await;
    let answer = asked.await.unwrap().sensitive[0].clone();
    assert_eq!(
        answer.status,
        SensitiveAnswerStatus::Provided,
        "the deposit must have been accepted for expiry to mean anything"
    );
    let reference = answer.reference;
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    assert!(
        svc.take_sensitive(&reference, &request_id, "owner", "workspace")
            .is_none(),
        "material outlived its collection deadline"
    );
}

#[tokio::test]
async fn a_cancelled_sensitive_request_deposits_nothing() {
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    let mut cancel = provide(id, "custody-canary-4");
    cancel.decision = "cancel".into();
    svc.respond_scoped(cancel, Some("owner"), Some("workspace"))
        .await;
    let response = asked.await.unwrap();
    assert_eq!(response.input, None);
    assert_eq!(response.decision, "cancel");
    // A decision answer deposits nothing and carries no sensitive answers.
    assert!(response.sensitive.is_empty());
    assert_no_canary(&svc, "custody-canary-4");
}

#[tokio::test]
async fn custody_material_never_reaches_persisted_history_or_pending_shards() {
    const CANARY: &str = "custody-canary-5";
    let temp = tempfile::tempdir().unwrap();
    let history_path = temp.path().join("history.json");
    let pending_path = temp.path().join("pending.json");
    let svc = Arc::new(
        service()
            .with_history_persist_path(&history_path)
            .with_pending_persist_path(&pending_path)
            .await,
    );
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    assert_eq!(
        svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
            .await,
        ScopedResponseResult::Accepted
    );
    let response = asked.await.unwrap();
    assert_eq!(
        response.sensitive[0].status,
        SensitiveAnswerStatus::Provided
    );
    let history = std::fs::read_to_string(&history_path).unwrap();
    assert!(
        history.contains(&response.request_id),
        "the resolution itself was persisted"
    );
    for path in [history_path, pending_path] {
        assert!(!std::fs::read_to_string(path).unwrap().contains(CANARY));
    }
    assert_no_canary(&svc, CANARY);
}

#[tokio::test]
async fn orphan_resolution_of_a_chat_password_persists_no_input() {
    const CANARY: &str = "custody-canary-6";
    let svc = service();
    let accepted = svc
        .accept_request(
            password_request(),
            UserRequestIdentityMode::FreshRandom,
            false,
        )
        .await
        .unwrap();
    let id = accepted.request.id;
    // Same state as after a restart with a lost pending snapshot.
    svc.pending.write().await.remove(&id);
    svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
        .await;
    assert_no_canary(&svc, CANARY);
    let history = svc.list_history_for_scope("owner", "workspace", None);
    assert!(history
        .iter()
        .filter_map(|r| r.response.as_ref())
        .all(|r| r.input.is_none()));
}

#[tokio::test]
async fn a_form_answer_deposits_one_entry_per_flagged_field() {
    let svc = Arc::new(service());
    let request = chat_request(
        "Log in to the portal",
        json!({
            "input_type": "form",
            "input_schema": { "questions": [
                { "id": "email", "prompt": "Email address", "input_type": "text" },
                { "id": "pw", "prompt": "Password", "input_type": "password" },
                { "id": "remember", "prompt": "Remember me?", "input_type": "choice" }
            ]}
        }),
    );
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    let request_id = id.clone();
    // The responder ships the whole form as one JSON object; the service
    // deposits the flagged fields and keeps the ordinary ones usable.
    let input = json!({ "email": "form-id-canary-7", "pw": "form-pw-canary-8", "remember": "yes" })
        .to_string();
    svc.respond_scoped(provide(id, &input), Some("owner"), Some("workspace"))
        .await;
    let response = asked.await.unwrap();

    assert_eq!(
        response.input.as_deref(),
        Some("remember: yes"),
        "ordinary answers remain usable"
    );
    let mut answers = response.sensitive.clone();
    answers.sort_by(|a, b| a.field.cmp(&b.field));
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].field.as_deref(), Some("email"));
    assert_eq!(answers[0].kind, SensitiveKind::LoginIdentifier);
    assert_eq!(answers[1].field.as_deref(), Some("pw"));
    assert_eq!(answers[1].kind, SensitiveKind::Password);
    assert_eq!(
        svc.take_sensitive(&answers[0].reference, &request_id, "owner", "workspace")
            .as_deref()
            .map(|v| v.as_str()),
        Some("form-id-canary-7")
    );
    assert_eq!(
        svc.take_sensitive(&answers[1].reference, &request_id, "owner", "workspace")
            .as_deref()
            .map(|v| v.as_str()),
        Some("form-pw-canary-8")
    );
    assert_no_canary(&svc, "form-id-canary-7");
    assert_no_canary(&svc, "form-pw-canary-8");
}

#[tokio::test]
async fn an_ordinary_request_still_returns_its_input() {
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move {
        caller
            .ask(chat_request(
                "What city should the meeting be in?",
                json!({ "input_type": "text" }),
            ))
            .await
    });
    let id = pending(&svc).await;
    svc.respond_scoped(provide(id, "Lisbon"), Some("owner"), Some("workspace"))
        .await;
    let response = asked.await.unwrap();
    assert_eq!(response.input.as_deref(), Some("Lisbon"));
    assert!(response.sensitive.is_empty());
}

#[tokio::test]
async fn an_unscoped_respond_cannot_deposit_material() {
    const CANARY: &str = "custody-canary-9";
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    // The legacy unscoped responder proves no scope, so material is refused
    // and the answer resolves as cancelled — never as an ordinary text answer.
    assert!(svc.respond(provide(id, CANARY)).await);
    let response = asked.await.unwrap();
    assert_eq!(response.input, None);
    assert!(response
        .sensitive
        .iter()
        .all(|a| a.status == SensitiveAnswerStatus::Cancelled));
    assert_no_canary(&svc, CANARY);
}

#[tokio::test]
async fn lifecycle_events_for_a_sensitive_answer_carry_no_value() {
    const CANARY: &str = "custody-canary-21";
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
    let mut events = broadcaster.subscribe();
    let svc = Arc::new(UserRequestService::new(broadcaster));
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    svc.respond_scoped(
        provide(id.clone(), CANARY),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    let response = asked.await.unwrap();
    assert_eq!(
        response.sensitive[0].status,
        SensitiveAnswerStatus::Provided
    );
    let is_resolution = |event: &RuntimeTransportEvent| {
        let rendered = serde_json::to_string(event).unwrap();
        rendered.contains(&id) && rendered.to_ascii_lowercase().contains("resolved")
    };
    let seen = events_until(&mut events, is_resolution).await;
    for event in &seen {
        let rendered = serde_json::to_string(event).unwrap();
        assert!(
            !rendered.contains(CANARY),
            "an event carried the value: {rendered}"
        );
    }
    assert!(
        seen.iter().any(is_resolution),
        "no resolution event was published for the request"
    );
}

// ---- Task 1.9: review remediation ------------------------------------------

fn decision_request(
    request_type: &str,
    question: &str,
    input_type: &str,
    options: &[&str],
) -> UserRequest {
    // An empty `input_type` is the production shape of the approval,
    // notification and relay producers: no type at all, options set.
    let context = if input_type.is_empty() {
        json!({})
    } else {
        json!({ "input_type": input_type })
    };
    let mut request = chat_request(question, context);
    request.request_type = request_type.into();
    request.options = options
        .iter()
        .map(|id| RequestOption {
            id: (*id).into(),
            label: (*id).into(),
            requires_input: false,
        })
        .collect();
    request
}

fn decide(id: String, decision: &str) -> UserResponse {
    UserResponse {
        request_id: id,
        decision: decision.into(),
        input: None,
        channel: "web".into(),
        sensitive: Vec::new(),
    }
}

#[tokio::test]
async fn an_approval_question_mentioning_a_token_still_resolves_its_decision() {
    let svc = Arc::new(service());
    let mut request = decision_request(
        "mcp_tool_approval",
        "The site asked for a two-factor code; approve calling tool github.create_token for repo magician?",
        "",
        &["approve_once", "deny"],
    );
    request.timeout_secs = 24 * 60 * 60;
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    assert!(
        svc.list_pending().await[0].sensitive.is_none(),
        "a decision-only request must not be classified"
    );
    svc.respond_scoped(decide(id, "approve_once"), Some("owner"), Some("workspace"))
        .await;
    let response = asked.await.unwrap();
    assert_eq!(response.decision, "approve_once");
    assert!(response.sensitive.is_empty());
    assert_eq!(
        svc.list_history_for_scope("owner", "workspace", None)[0]
            .request
            .timeout_secs,
        24 * 60 * 60,
        "a decision prompt keeps its own window"
    );
}

#[tokio::test]
async fn a_sensitive_request_answered_with_an_option_keeps_the_option() {
    let svc = Arc::new(service());
    let mut request = password_request();
    request.options = vec![
        RequestOption {
            id: "use_saved".into(),
            label: "Use the saved one".into(),
            requires_input: false,
        },
        RequestOption {
            id: "provide_input".into(),
            label: "Type it".into(),
            requires_input: true,
        },
    ];
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    svc.respond_scoped(decide(id, "use_saved"), Some("owner"), Some("workspace"))
        .await;
    let response = asked.await.unwrap();
    assert_eq!(
        response.decision, "use_saved",
        "a decision on a sensitive request is never rewritten"
    );
    assert!(
        response.sensitive.is_empty(),
        "a decision answer carries no sensitive answers"
    );
    assert_eq!(response.input, None);
}

#[tokio::test]
async fn a_choice_question_mentioning_pin_resolves_the_selected_id() {
    let svc = Arc::new(service());
    let request = decision_request(
        "need_user_input",
        "Pin the note or archive it?",
        "choice",
        &["pin", "archive"],
    );
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    svc.respond_scoped(decide(id, "pin"), Some("owner"), Some("workspace"))
        .await;
    assert_eq!(asked.await.unwrap().decision, "pin");
}

#[tokio::test]
async fn a_secure_browser_confirm_on_a_pin_host_still_allows_once() {
    let svc = Arc::new(service());
    let mut request = chat_request(
        "Fill the login form on https://pin.example.com with the credential you just entered?",
        json!({}),
    );
    request.source = "browser_secure_prompt_fill".into();
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask_secure_confirmation(request).await });
    let id = pending(&svc).await;
    svc.respond_scoped(decide(id, "allow_once"), Some("owner"), Some("workspace"))
        .await;
    assert!(
        asked.await.unwrap(),
        "the one-time fill confirmation was turned into a cancel"
    );
}

#[tokio::test]
async fn a_text_question_that_only_mentions_a_token_in_prose_stays_ordinary() {
    let svc = Arc::new(service());
    let request = chat_request(
        "Which tokens should the summary keep?",
        json!({ "input_type": "text" }),
    );
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    svc.respond_scoped(
        provide(id, "the first three"),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    let response = asked.await.unwrap();
    assert_eq!(response.input.as_deref(), Some("the first three"));
    assert!(response.sensitive.is_empty());
}

#[tokio::test]
async fn a_pending_row_restored_without_a_spec_is_classified_on_restore() {
    const CANARY: &str = "custody-canary-22";
    let temp = tempfile::tempdir().unwrap();
    let pending_path = temp.path().join("pending.json");
    let history_path = temp.path().join("history.json");
    let id = {
        let svc = Arc::new(
            service()
                .with_history_persist_path(&history_path)
                .with_pending_persist_path(&pending_path)
                .await,
        );
        let mut request = password_request();
        request.timeout_secs = 600;
        let caller = svc.clone();
        let asked = tokio::spawn(async move { caller.ask(request).await });
        let id = pending(&svc).await;
        asked.abort();
        let _ = asked.await;
        stop_in_process_owners(&svc).await;
        id
    };
    // Shards written by a pre-contract binary carry no `sensitive` key.
    for path in [&pending_path, &history_path] {
        let mut rows: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        fn strip(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    map.remove("sensitive");
                    map.values_mut().for_each(strip);
                },
                serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
                _ => {},
            }
        }
        strip(&mut rows);
        std::fs::write(path, serde_json::to_string(&rows).unwrap()).unwrap();
        assert!(!std::fs::read_to_string(path)
            .unwrap()
            .contains("\"sensitive\""));
    }

    let svc = Arc::new(
        service()
            .with_history_persist_path(&history_path)
            .with_pending_persist_path(&pending_path)
            .await,
    );
    let restored = svc.list_pending().await;
    assert_eq!(restored.len(), 1, "the snapshot should rehydrate the entry");
    assert_eq!(restored[0].id, id);
    assert!(
        restored[0].sensitive.is_some(),
        "restore must classify a row without a spec"
    );
    svc.respond_scoped(provide(id, CANARY), Some("owner"), Some("workspace"))
        .await;
    assert_no_canary(&svc, CANARY);
    assert!(!std::fs::read_to_string(&history_path)
        .unwrap()
        .contains(CANARY));
}

#[tokio::test]
async fn an_idempotent_resubmit_of_a_classified_request_replays() {
    let svc = service();
    let mut request = password_request();
    request.id = "owner-notify:password-1".into();
    let first = svc.submit_nonblocking(request.clone()).await.unwrap();
    assert!(matches!(first, UserRequestSubmission::Accepted { .. }));
    let second = svc.submit_nonblocking(request).await.unwrap();
    assert!(
        matches!(second, UserRequestSubmission::IdempotentReplay { .. }),
        "{second:?}"
    );
}

#[tokio::test]
async fn a_one_time_spec_clamps_the_request_timeout_at_accept() {
    let svc = Arc::new(service());
    let mut request = chat_request(
        "Enter the verification code we sent to your phone",
        json!({ "input_type": "text" }),
    );
    request.timeout_secs = 24 * 60 * 60;
    let caller = svc.clone();
    let _asked = tokio::spawn(async move { caller.ask(request).await });
    let _ = pending(&svc).await;
    let pending = svc.list_pending().await.remove(0);
    assert!(pending.sensitive.as_ref().unwrap().one_time);
    assert!(
        pending.timeout_secs <= 180,
        "request outlived its one-time collection window: {}",
        pending.timeout_secs
    );
}

#[tokio::test]
async fn retiring_a_request_forgets_its_custody() {
    const CANARY: &str = "custody-canary-23";
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    svc.respond_scoped(
        provide(id.clone(), CANARY),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    let reference = asked.await.unwrap().sensitive[0].reference.clone();
    svc.retire_sensitive_for_request(&id);
    assert!(svc
        .take_sensitive(&reference, &id, "owner", "workspace")
        .is_none());
}

#[test]
fn custody_hold_is_bounded_even_for_a_day_long_request() {
    let now = 1_700_000_000_000;
    let day = now + 24 * 60 * 60 * 1000;
    assert_eq!(custody_hold_deadline_ms(day, now), now + 15 * 60 * 1000);
    assert_eq!(custody_hold_deadline_ms(now + 1000, now), now + 1000);
}

// ---- P3 Task 3.1: the spec is published on every announcement -------------

#[tokio::test]
async fn hitl_requested_carries_the_value_free_spec_of_a_sensitive_ask() {
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
    let mut events = broadcaster.subscribe();
    let svc = Arc::new(UserRequestService::new(broadcaster));
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    // The listing a client polls carries the spec.
    let listed = svc.list_pending_for_scope("owner", "workspace").await;
    assert_eq!(
        listed[0].sensitive.as_ref().map(|spec| spec.kind),
        Some(Some(SensitiveKind::Password))
    );
    svc.respond_scoped(
        provide(id.clone(), "p3-publish-canary-1"),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    asked.await.unwrap();
    // The announcement carries the same spec and never the value.
    let seen = events_until(&mut events, |event| {
        matches!(event, RuntimeTransportEvent::HitlRequested { correlation_id, .. } if correlation_id == &id)
    })
    .await;
    let mut published = None;
    for event in &seen {
        let rendered = serde_json::to_string(event).unwrap();
        assert!(
            !rendered.contains("p3-publish-canary-1"),
            "an event carried the value: {rendered}"
        );
        if let RuntimeTransportEvent::HitlRequested {
            correlation_id,
            input_schema,
            ..
        } = event
        {
            if correlation_id == &id {
                published = input_schema.clone();
            }
        }
    }
    let schema = published.expect("the ask was announced");
    let spec = schema
        .get("sensitive")
        .expect("the announcement carries the spec");
    assert_eq!(spec["kind"], "password");
    assert_eq!(spec["provenance"], "typed_input");
    assert!(spec["collection_deadline_ms"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn hitl_requested_for_a_decision_only_ask_carries_no_spec() {
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
    let mut events = broadcaster.subscribe();
    let svc = Arc::new(UserRequestService::new(broadcaster));
    let caller = svc.clone();
    let request = decision_request(
        "mcp_tool_approval",
        "Allow the token tool?",
        "",
        &["allow_once", "deny"],
    );
    let asked = tokio::spawn(async move { caller.ask(request).await });
    let id = pending(&svc).await;
    svc.respond_scoped(
        decide(id.clone(), "allow_once"),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    asked.await.unwrap();
    let seen = events_until(&mut events, |event| {
        matches!(event, RuntimeTransportEvent::HitlRequested { correlation_id, .. } if correlation_id == &id)
    })
    .await;
    let mut announced = false;
    for event in &seen {
        if let RuntimeTransportEvent::HitlRequested {
            correlation_id,
            input_schema,
            ..
        } = event
        {
            if correlation_id == &id {
                announced = true;
                assert!(input_schema
                    .as_ref()
                    .and_then(|schema| schema.get("sensitive"))
                    .is_none());
            }
        }
    }
    assert!(announced, "the decision ask was announced");
}

// ---- P3 Task 3.7: retirement and expiry ------------------------------------

#[tokio::test]
async fn untaken_material_past_its_hold_is_swept_by_the_next_service_call() {
    const CANARY: &str = "custody-sweep-canary-31";
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    svc.respond_scoped(
        provide(id.clone(), CANARY),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    asked.await.unwrap();
    assert_eq!(svc.sensitive_custody.held_count(), 1);
    let row_status = |svc: &UserRequestService| {
        svc.list_history_for_scope("owner", "workspace", None)
            .into_iter()
            .find(|row| row.request.id == id)
            .and_then(|row| row.response)
            .and_then(|response| response.sensitive.first().map(|answer| answer.status))
    };
    assert_eq!(row_status(&svc), Some(SensitiveAnswerStatus::Provided));
    // Nothing is due yet.
    svc.sweep_expired_custody_at(chrono::Utc::now().timestamp_millis());
    assert_eq!(svc.sensitive_custody.held_count(), 1);
    assert_eq!(row_status(&svc), Some(SensitiveAnswerStatus::Provided));
    // Past the hold, the next sweep drops it — the service sweeps on every
    // accept and every response, so a later ask is enough — and the history
    // row says `expired` where it said `provided`.
    let far_future = chrono::Utc::now().timestamp_millis() + 16 * 60 * 1000;
    svc.sweep_expired_custody_at(far_future);
    assert_eq!(svc.sensitive_custody.held_count(), 0);
    assert_eq!(row_status(&svc), Some(SensitiveAnswerStatus::Expired));
    assert_no_canary(&svc, CANARY);
}

#[tokio::test]
async fn an_orphan_decision_on_a_sensitive_request_keeps_the_decision() {
    let svc = service();
    let mut request = password_request();
    request.options = vec![
        RequestOption {
            id: "use_saved".into(),
            label: "Use the saved login".into(),
            requires_input: false,
        },
        RequestOption {
            id: "cancel_login".into(),
            label: "Cancel".into(),
            requires_input: false,
        },
    ];
    let accepted = svc
        .accept_request(request, UserRequestIdentityMode::FreshRandom, false)
        .await
        .unwrap();
    let id = accepted.request.id;
    svc.pending.write().await.remove(&id);
    svc.respond_scoped(
        decide(id.clone(), "use_saved"),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    let history = svc.list_history_for_scope("owner", "workspace", None);
    let record = history
        .iter()
        .find(|r| r.request.id == id)
        .expect("orphan resolution persisted");
    let response = record.response.as_ref().expect("a response");
    assert_eq!(
        response.decision, "use_saved",
        "the operator's choice survives the orphan path"
    );
    assert!(response.input.is_none());

    // An answer that WAS the material has nothing left to deliver and becomes a cancel.
    let accepted = svc
        .accept_request(
            password_request(),
            UserRequestIdentityMode::FreshRandom,
            false,
        )
        .await
        .unwrap();
    let id = accepted.request.id;
    svc.pending.write().await.remove(&id);
    svc.respond_scoped(
        provide(id.clone(), "custody-orphan-canary-32"),
        Some("owner"),
        Some("workspace"),
    )
    .await;
    let history = svc.list_history_for_scope("owner", "workspace", None);
    let record = history.iter().find(|r| r.request.id == id).unwrap();
    assert_eq!(record.response.as_ref().unwrap().decision, "cancel");
    assert_no_canary(&svc, "custody-orphan-canary-32");
}

/// The record names the secure path, not the relay — except the runtime's
/// own automatic answerers, so the audit trail can tell a retrieved code
/// from a typed one.
#[tokio::test]
async fn an_automatic_answerers_channel_stays_on_the_record() {
    let svc = Arc::new(service());
    let caller = svc.clone();
    let asked = tokio::spawn(async move { caller.ask(password_request()).await });
    let id = pending(&svc).await;
    let response = UserResponse {
        channel: VERIFICATION_CODE_RESOLVER_CHANNEL.into(),
        ..provide(id, "retrieved-canary")
    };
    assert_eq!(
        svc.respond_scoped(response, Some("owner"), Some("workspace"))
            .await,
        ScopedResponseResult::Accepted
    );
    let response = asked.await.unwrap();
    assert_eq!(response.channel, VERIFICATION_CODE_RESOLVER_CHANNEL);
    assert_eq!(response.input, None);
    assert_eq!(
        recorded_sensitive_channel("telegram"),
        SECURE_ANSWER_CHANNEL
    );
    assert_eq!(
        recorded_sensitive_channel(ANDROID_NOTIFICATION_CHANNEL),
        ANDROID_NOTIFICATION_CHANNEL
    );
}
