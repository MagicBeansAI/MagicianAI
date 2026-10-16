//! Private material channel for built-in one-time credential delivery.
//! Public UserResponse, durable history, and lifecycle events carry no values.
use super::*;

pub(super) const SECURE_CONFIRM_REQUEST: &str = "secure_browser_confirm";

pub(super) const SECURE_INPUT_REQUEST: &str = "secure_browser_input";

/// The channel a sensitive answer is recorded under: the secure path, not
/// the relay it arrived through — except for the runtime's own automatic
/// answerers, whose names stay on the record so the audit trail can tell a
/// retrieved code from a typed one. Those names are refused from any
/// outside caller by the API (`verification_code_resolver` is in-process;
/// `android_notification` needs the paired device's own credential and its
/// verification-code grant).
pub const SECURE_ANSWER_CHANNEL: &str = "secure_ui";
pub const VERIFICATION_CODE_RESOLVER_CHANNEL: &str = "verification_code_resolver";
pub const ANDROID_NOTIFICATION_CHANNEL: &str = "android_notification";

/// The channel to record for a sensitive answer that arrived on `channel`.
pub fn recorded_sensitive_channel(channel: &str) -> String {
    if matches!(
        channel,
        VERIFICATION_CODE_RESOLVER_CHANNEL | ANDROID_NOTIFICATION_CHANNEL
    ) {
        channel.to_string()
    } else {
        SECURE_ANSWER_CHANNEL.to_string()
    }
}

/// What kind of credential material a sensitive answer is. Decides custody
/// lifetime and how a consumer may lower the reference into a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveKind {
    /// A username or e-mail collected as part of an authentication bundle.
    LoginIdentifier,
    Password,
    /// A one-time code; never reusable, never saved.
    Otp,
    /// Sensitive by a conservative signal (prompt wording); handled as secret.
    Other,
}

/// Which trusted signal classified the request. Recorded for review; a
/// heuristic classification can raise sensitivity but never lower a typed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveProvenance {
    /// A trusted in-process producer set the spec itself.
    Producer,
    /// The request's typed input (`input_type: password`).
    TypedInput,
    /// A form question typed or flagged sensitive in the request schema.
    FormSchema,
    /// Prompt/name wording only.
    Heuristic,
}

/// One sensitive field of a form request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveField {
    pub id: String,
    pub kind: SensitiveKind,
}

/// Server-owned sensitivity contract for a pending request. Metadata only —
/// it never carries a value — so it is serialized into the pending and
/// history shards and published with lifecycle events, which is how clients
/// know to render masked fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveInputSpec {
    /// Kind of the whole answer when the request collects one value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SensitiveKind>,
    /// Per-field kinds when the request collects a form. A field absent here
    /// is ordinary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SensitiveField>,
    pub provenance: SensitiveProvenance,
    /// One bound use (browser JIT, OTP). `false` keeps the execution-scoped
    /// lifetime.
    #[serde(default)]
    pub one_time: bool,
    /// Epoch millis. Material is unavailable after this even if never taken.
    pub collection_deadline_ms: i64,
    /// The authentication challenge this ask answers (P4): set when a typed
    /// challenge outcome raised the ask, so the material the answer becomes
    /// is bound to it and a claim for another challenge is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_id: Option<String>,
    #[serde(default)]
    pub revision: u32,
    /// Where the answer will be delivered (P4): the host for an HTTP
    /// challenge, the origin for a browser one, the program for a CLI one.
    /// Bound into one-time material at registration; the delivering adapter
    /// must name exactly this destination. `None` binds no destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_destination: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveAnswerStatus {
    Provided,
    Cancelled,
    Expired,
    Unavailable,
}

/// The value-free status a public `UserResponse` carries instead of `input`
/// for a sensitive answer. One per sensitive value: a single-value answer has
/// one with `field: None`; a form has one per flagged field. The reference
/// identifies custody material; it is not bearer authority to read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveAnswer {
    pub reference: String,
    pub kind: SensitiveKind,
    pub status: SensitiveAnswerStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl std::fmt::Debug for UserResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserResponse")
            .field("request_id", &self.request_id)
            .field("decision", &self.decision)
            .field("input", &self.input.as_ref().map(|_| "[redacted]"))
            .field("channel", &self.channel)
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

impl UserRequestService {
    /// In-process only: never expose this return type as an agent tool.
    /// Cancellation closes the receiver; late submissions are discarded.
    /// Deliberately avoids plane/MCP form elicitation: use the authenticated UI.
    pub(crate) async fn ask_sensitive_once(
        self: &Arc<Self>,
        mut request: UserRequest,
    ) -> Option<Zeroizing<String>> {
        if request.principal.is_empty() || request.workspace.is_empty() {
            return None;
        }
        request.request_type = SECURE_INPUT_REQUEST.into();
        request.context["input_type"] = serde_json::json!("password");
        request.context["credential_lifetime"] = serde_json::json!("one_time");
        request.default_on_timeout = "cancel".into();
        request.timeout_secs = request.timeout_secs.clamp(1, 180);
        let (tx, rx) = oneshot::channel();
        let accepted = self
            .accept_request_with_sensitive_receiver(
                request,
                UserRequestIdentityMode::FreshRandom,
                false,
                Some(tx),
            )
            .await
            .ok()?;
        // Keep the ordinary receiver alive for the first-response gate, but
        // never return its input to a model or accept data from that channel.
        let mut guard = PendingGuard::new(self.clone(), &accepted.request);
        let _status_rx = accepted.response_rx;
        let result = rx.await.ok();
        guard.id = None;
        result
    }
}

impl UserRequestService {
    pub(crate) async fn ask_secure_confirmation(
        self: &Arc<Self>,
        mut request: UserRequest,
    ) -> bool {
        request.request_type = SECURE_CONFIRM_REQUEST.into();
        request.context["input_type"] = serde_json::json!("choice");
        request.default_on_timeout = "cancel".into();
        let Ok(mut accepted) = self
            .accept_request(request, UserRequestIdentityMode::FreshRandom, false)
            .await
        else {
            return false;
        };
        let mut guard = PendingGuard::new(self.clone(), &accepted.request);
        let result = match accepted.response_rx.take() {
            Some(rx) => rx.await.is_ok_and(|r| r.decision == "allow_once"),
            None => false,
        };
        guard.id = None;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const CANARY: &str = "jit-only-credential-canary-91";
    fn request() -> UserRequest {
        UserRequest {
            id: String::new(),
            request_type: SECURE_INPUT_REQUEST.into(),
            question: "Private input".into(),
            options: vec![],
            principal: "owner".into(),
            workspace: "workspace".into(),
            context: json!({}),
            source: "browser_secure_prompt_fill".into(),
            execution_id: Some("execution".into()),
            task_id: None,
            timeout_secs: 60,
            default_on_timeout: "cancel".into(),
            created_at: 0,
            sensitive: None,
        }
    }
    fn service() -> UserRequestService {
        UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
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
    fn answer(id: String) -> UserResponse {
        UserResponse {
            request_id: id,
            decision: "provide_input".into(),
            input: Some(CANARY.into()),
            channel: "web".into(),
            sensitive: Vec::new(),
        }
    }
    fn assert_clean(svc: &UserRequestService) {
        let history = svc.list_history_for_scope("owner", "workspace", None);
        assert!(!serde_json::to_string(&history).unwrap().contains(CANARY));
        assert!(history
            .iter()
            .filter_map(|r| r.response.as_ref())
            .all(|r| r.input.is_none()));
    }
    #[tokio::test]
    async fn jit_private_answer_never_enters_history_or_public_response() {
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
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        assert_eq!(
            svc.respond_scoped(answer(id.clone()), Some("owner"), Some("workspace"))
                .await,
            ScopedResponseResult::Accepted
        );
        assert_eq!(
            task.await.unwrap().as_deref().map(|v| v.as_str()),
            Some(CANARY)
        );
        assert_clean(&svc);
        for path in [history_path, pending_path] {
            assert!(!std::fs::read_to_string(path).unwrap().contains(CANARY));
        }
        assert_eq!(
            svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
                .await,
            ScopedResponseResult::AlreadyResolved
        );
        assert_clean(&svc);
    }
    #[tokio::test]
    async fn jit_persistence_failure_does_not_deliver_or_retain_material() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.json");
        let svc = Arc::new(service().with_history_persist_path(&path));
        let caller = svc.clone();
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            svc.respond_scoped(answer(id.clone()), Some("owner"), Some("workspace"))
                .await,
            ScopedResponseResult::PersistenceUnavailable
        );
        assert!(!task.is_finished());
        assert_clean(&svc);
        std::fs::remove_dir(&path).unwrap();
        assert_eq!(
            svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
                .await,
            ScopedResponseResult::Accepted
        );
        assert!(task.await.unwrap().is_some());
        assert!(!std::fs::read_to_string(path).unwrap().contains(CANARY));
    }

    #[tokio::test]
    async fn jit_scope_mismatch_cannot_consume_answer() {
        let svc = Arc::new(service());
        let caller = svc.clone();
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        assert_eq!(
            svc.respond_scoped(answer(id.clone()), Some("intruder"), Some("workspace"))
                .await,
            ScopedResponseResult::ScopeMismatch
        );
        assert!(!task.is_finished());
        svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
            .await;
        assert!(task.await.unwrap().is_some());
        assert_clean(&svc);
    }
    #[tokio::test]
    async fn jit_abandoned_receiver_discards_late_material() {
        let svc = Arc::new(service());
        let caller = svc.clone();
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        task.abort();
        let _ = task.await;
        svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
            .await;
        assert_clean(&svc);
        assert_eq!(
            svc.list_history_for_scope("owner", "workspace", None)[0]
                .response
                .as_ref()
                .unwrap()
                .decision,
            "cancel"
        );
    }
    #[tokio::test]
    async fn jit_restored_request_cannot_collect_or_persist_credentials() {
        let svc = service();
        let accepted = svc
            .accept_request(request(), UserRequestIdentityMode::FreshRandom, false)
            .await
            .unwrap();
        let id = accepted.request.id;
        drop(accepted.response_rx);
        // Same state as a restored request: metadata exists, no private receiver.
        svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
            .await;
        assert_clean(&svc);
    }
    #[tokio::test]
    async fn jit_orphan_after_restart_discards_material() {
        let svc = service();
        let accepted = svc
            .accept_request(request(), UserRequestIdentityMode::FreshRandom, false)
            .await
            .unwrap();
        let id = accepted.request.id;
        svc.pending.write().await.remove(&id);
        svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
            .await;
        assert_clean(&svc);
    }
    #[tokio::test]
    async fn jit_expired_request_cannot_accept_material() {
        let svc = Arc::new(service());
        let caller = svc.clone();
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        svc.pending
            .write()
            .await
            .get_mut(&id)
            .unwrap()
            .request
            .created_at = 0;
        svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
            .await;
        assert!(task.await.unwrap().is_none());
        assert_clean(&svc);
    }
    #[tokio::test]
    async fn jit_first_response_wins_and_debug_redacts() {
        assert!(!format!("{:?}", answer("id".into())).contains(CANARY));
        let svc = Arc::new(service());
        let caller = svc.clone();
        let task = tokio::spawn(async move { caller.ask_sensitive_once(request()).await });
        let id = pending(&svc).await;
        let (a, b) = tokio::join!(
            svc.respond_scoped(answer(id.clone()), Some("owner"), Some("workspace")),
            svc.respond_scoped(answer(id), Some("owner"), Some("workspace"))
        );
        assert!(matches!(
            (a, b),
            (
                ScopedResponseResult::Accepted,
                ScopedResponseResult::AlreadyResolved
            ) | (
                ScopedResponseResult::AlreadyResolved,
                ScopedResponseResult::Accepted
            )
        ));
        assert!(task.await.unwrap().is_some());
        assert_clean(&svc);
    }

    /// P0 regression fixture (plan §2 "Generic chat HITL", §8 "Password through
    /// generic chat HITL"). Mirrors what `ChatService::dispatch_chat_need_user_input`
    /// builds and answers it through the ordinary `respond` path. Expected to
    /// fail until the P1 shared sensitive-answer boundary lands: today the
    /// secure custody route is keyed on `request_type == secure_browser_input`,
    /// so a password asked by chat resolves as ordinary text.
    #[tokio::test]
    async fn p0_chat_password_answer_never_enters_history_or_ordinary_response() {
        const CHAT_CANARY: &str = "p0-chat-canary-1";
        let svc = Arc::new(service());
        let mut request = request();
        request.request_type = "need_user_input".into();
        request.source = "chat".into();
        request.execution_id = Some("chat-session-1".into());
        request.context = json!({
            "input_type": "password",
            "chat_session_id": "chat-session-1",
            "agent_id": "presto",
        });
        let caller = svc.clone();
        let asked = tokio::spawn(async move { caller.ask(request).await });
        let id = pending(&svc).await;
        let mut answer = answer(id);
        answer.input = Some(CHAT_CANARY.into());
        assert!(svc.respond(answer).await);
        let response = asked.await.unwrap();
        assert!(
            !response
                .input
                .as_deref()
                .unwrap_or_default()
                .contains(CHAT_CANARY),
            "the ordinary UserResponse handed back to a model-facing caller carried the password"
        );
        let history = svc.list_history_for_scope("owner", "workspace", None);
        assert!(
            !serde_json::to_string(&history)
                .unwrap()
                .contains(CHAT_CANARY),
            "request history retained the password"
        );
    }

    // ---- P1 Task 1.1: sensitivity types on request and response ----------

    fn login_spec() -> SensitiveInputSpec {
        SensitiveInputSpec {
            kind: Some(SensitiveKind::Password),
            fields: vec![],
            provenance: SensitiveProvenance::TypedInput,
            one_time: true,
            collection_deadline_ms: 1_700_000_000_000,
            challenge_id: None,
            revision: 0,
            expected_destination: None,
        }
    }

    #[test]
    fn sensitive_spec_round_trips_and_is_absent_when_none() {
        let mut with_spec = request();
        with_spec.sensitive = Some(login_spec());
        let json = serde_json::to_value(&with_spec).unwrap();
        assert_eq!(json["sensitive"]["kind"], serde_json::json!("password"));
        assert_eq!(json["sensitive"]["one_time"], serde_json::json!(true));
        let back: UserRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.sensitive, Some(login_spec()));

        let without = request();
        let json = serde_json::to_value(&without).unwrap();
        assert!(
            json.get("sensitive").is_none(),
            "None must not serialize a key"
        );
        // A pending/history shard written before this field existed still loads.
        let legacy: UserRequest = serde_json::from_value(json).unwrap();
        assert_eq!(legacy.sensitive, None);
    }

    #[test]
    fn sensitive_answer_on_a_response_is_value_free() {
        let response = UserResponse {
            request_id: "r1".into(),
            decision: "provided".into(),
            input: None,
            channel: "secure_ui".into(),
            sensitive: vec![SensitiveAnswer {
                reference: "sr_abc".into(),
                kind: SensitiveKind::Password,
                status: SensitiveAnswerStatus::Provided,
                field: None,
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(
            json["sensitive"][0]["reference"],
            serde_json::json!("sr_abc")
        );
        assert_eq!(
            json["sensitive"][0]["status"],
            serde_json::json!("provided")
        );
        assert!(json.get("input").is_none());
        assert!(json["sensitive"][0].get("field").is_none());
        let back: UserResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.sensitive.len(), 1);

        // A response without any sensitive answer serializes no key at all and
        // an old shard without the key still loads.
        let plain = answer("r2".into());
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("sensitive").is_none());
        let legacy: UserResponse = serde_json::from_value(json).unwrap();
        assert!(legacy.sensitive.is_empty());
        // Debug still hides the raw input.
        assert!(!format!("{plain:?}").contains(CANARY));
    }
}

// Drop runs on task cancellation or the enclosing operation deadline. Retire
// metadata promptly; a late response sees closed receivers even before this
// asynchronous cleanup wins the service lock.
struct PendingGuard {
    service: Arc<UserRequestService>,
    id: Option<String>,
    principal: String,
    workspace: String,
}
impl PendingGuard {
    fn new(service: Arc<UserRequestService>, request: &UserRequest) -> Self {
        Self {
            service,
            id: Some(request.id.clone()),
            principal: request.principal.clone(),
            workspace: request.workspace.clone(),
        }
    }
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        let (Some(id), Ok(runtime)) = (self.id.take(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let service = self.service.clone();
        let principal = self.principal.clone();
        let workspace = self.workspace.clone();
        runtime.spawn(async move {
            service
                .respond_scoped(
                    UserResponse {
                        request_id: id,
                        decision: "cancel".into(),
                        input: None,
                        channel: "secure_ui".into(),
                        sensitive: Vec::new(),
                    },
                    Some(&principal),
                    Some(&workspace),
                )
                .await;
        });
    }
}
