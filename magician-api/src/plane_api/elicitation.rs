//! One POST stream and one continuation per client call. Answers are routed by
//! authenticated grant + server-issued session + server-issued elicitation ID.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use magician::magician_v2::execution::agentic::types::{UserInputType, UserInputValue};
use magician::magician_v2::execution::plane::input::InputForm;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) const INPUT_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const MAX_ACTIVE_CALLS: usize = 32;

#[derive(Clone, Copy, Default)]
pub(super) struct ClientCapabilities {
    pub form: bool,
    pub url: bool,
}

impl ClientCapabilities {
    pub fn supports(&self, mode: &str) -> bool {
        match mode {
            "form" => self.form,
            "url" => self.url,
            _ => false,
        }
    }
    pub fn from_initialize(body: &Value) -> Self {
        let Some(elicitation) = body
            .pointer("/params/capabilities/elicitation")
            .and_then(Value::as_object)
        else {
            return Self::default();
        };
        Self {
            form: elicitation.is_empty() || elicitation.get("form").is_some_and(Value::is_object),
            url: elicitation.get("url").is_some_and(Value::is_object),
        }
    }
}

struct PendingAnswer {
    id: String,
    form: InputForm,
    sender: oneshot::Sender<Result<UserInputValue, String>>,
}

pub(super) struct CallState {
    pub session: String,
    pub request_id: Value,
    pub cancelled: CancellationToken,
    tool: String,
    output: mpsc::Sender<String>,
    pending: Mutex<Option<PendingAnswer>>,
}

impl CallState {
    pub fn new(
        session: String,
        request_id: Value,
        tool: String,
    ) -> (Arc<Self>, mpsc::Receiver<String>) {
        let (output, receiver) = mpsc::channel(8);
        (
            Arc::new(Self {
                session,
                request_id,
                tool,
                cancelled: CancellationToken::new(),
                output,
                pending: Mutex::new(None),
            }),
            receiver,
        )
    }

    pub async fn send(&self, message: &Value) {
        // A disconnected stream does not cancel an operation. Its eventual
        // result remains available in the request replay cache.
        let _ = self.output.try_send(super::sse_event(message));
    }

    pub async fn ask(
        &self,
        message: &str,
        input_type: UserInputType,
    ) -> Result<UserInputValue, String> {
        let form = InputForm::new(input_type)?;
        let id = format!("pltel_{}", Uuid::new_v4().simple());
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": {
            "mode": "form", "message": message, "requestedSchema": form.schema()
        }});
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if pending.is_some() {
                return Err("this call already has a pending question".into());
            }
            *pending = Some(PendingAnswer {
                id: id.clone(),
                form,
                sender,
            });
        }
        let _pending_guard = PendingGuard {
            call: self,
            id: id.clone(),
        };
        magician::magician_v2::execution::plane::record(&self.session,
            magician::magician_v2::execution::plane::TerminalLedgerEntry {
                tool: self.tool.clone(), timestamp_ms: chrono::Utc::now().timestamp_millis(),
                outcome: magician::magician_v2::execution::plane::TerminalLedgerOutcome::ElicitationRequested { request_id: id.clone() },
            });
        self.send(&request).await;
        let result = tokio::select! {
            _ = self.cancelled.cancelled() => Err("request cancelled".into()),
            answer = tokio::time::timeout(INPUT_TIMEOUT, receiver) => match answer {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err("input channel closed".into()),
                Err(_) => Err("input request timed out".into()),
            },
        };
        result
    }

    /// False means this call does not own an active question with this ID.
    /// A malformed matching answer
    /// ends only this question with an error; it never executes a continuation.
    pub fn answer(&self, session: &str, body: &Value) -> bool {
        if session != self.session || self.cancelled.is_cancelled() {
            return false;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        if !pending
            .as_ref()
            .is_some_and(|p| body.get("id").and_then(Value::as_str) == Some(p.id.as_str()))
        {
            return false;
        }
        let pending = pending.take().expect("matched pending answer");
        let answer = if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || body.get("method").is_some()
        {
            Err("invalid elicitation response envelope".into())
        } else if body.get("error").is_some() {
            Err("client could not complete elicitation".into())
        } else {
            pending
                .form
                .decode(body.get("result").unwrap_or(&Value::Null))
        };
        let _ = pending.sender.send(answer);
        true
    }
}

struct PendingGuard<'a> {
    call: &'a CallState,
    id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let mut pending = self.call.pending.lock().unwrap_or_else(|p| p.into_inner());
        if pending.as_ref().is_some_and(|p| p.id == self.id) {
            pending.take();
        }
    }
}
