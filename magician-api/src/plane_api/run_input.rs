//! Typed input continuations and explicit interaction with plane-launched runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use magician::config::PlaneConfig;
use magician::magician_v2::artifact_v2::service::V3ReadApi;
use magician::magician_v2::auth::{middleware::AuthRuntime, ScopeRef};
use magician::magician_v2::execution::agentic::types::{UserInputType, UserInputValue};
use magician::magician_v2::execution::plane::{
    catalog::is_callable_configured,
    dispatch::plane_execute_approved_capture,
    grant::PlanePendingApproval,
    input::{
        input_type_from_arguments, pause_input_type, service_input_type, service_response,
        InputChannel,
    },
    plane_tools_call_configured, PlaneGrant, TerminalLedgerEntry, TerminalLedgerOutcome,
};
use magician::magician_v2::user_requests::service::ScopedResponseResult;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::elicitation::CallState;
use super::{grant_authority_key, resolve_plane_grant, GrantReplayState};
use crate::web_api::{AgenticResumeRequest, MagicianV2Api};

#[derive(Clone)]
pub(super) struct RunOrigin {
    pub session: String,
    pub task_id: String,
    pub waiter: Option<Value>,
}

struct RunWaitGuard {
    replay: Arc<Mutex<GrantReplayState>>,
    execution: String,
    request_id: Value,
}

impl Drop for RunWaitGuard {
    fn drop(&mut self) {
        let replay = self.replay.clone();
        let execution = self.execution.clone();
        let id = self.request_id.clone();
        actix_web::rt::spawn(async move {
            if let Some(origin) = replay.lock().await.run_origins.get_mut(&execution) {
                if origin.waiter.as_ref() == Some(&id) {
                    origin.waiter = None;
                }
            }
        });
    }
}

/// Service-backed asks revalidate the bearer immediately before returning the
/// answer to their owning service, including revocation while a form was open.
pub(super) struct AuthorizedInputChannel {
    pub call: Arc<CallState>,
    pub token: String,
    pub grant: PlaneGrant,
    pub auth: Option<actix_web::web::Data<AuthRuntime>>,
}

#[async_trait::async_trait]
impl InputChannel for AuthorizedInputChannel {
    async fn ask(
        &self,
        question: &str,
        input_type: UserInputType,
    ) -> Result<UserInputValue, String> {
        let value = self.call.ask(question, input_type).await?;
        revalidate(
            &self.token,
            &self.grant,
            self.auth.as_ref().map(|a| a.get_ref()),
        )
        .await?;
        if self.call.cancelled.is_cancelled() {
            return Err("request cancelled".into());
        }
        Ok(value)
    }
}

fn error(message: impl Into<String>) -> Value {
    json!({"isError": true, "content": [{"type": "text", "text": message.into()}]})
}

fn answered(value: UserInputValue) -> Value {
    json!({"isError": false, "content": [{"type":"text", "text": serde_json::to_string(&value).unwrap_or_default()}], "structuredContent": {"answer": value}})
}

async fn revalidate(
    token: &str,
    original: &PlaneGrant,
    auth: Option<&AuthRuntime>,
) -> Result<PlaneGrant, String> {
    if original.is_revoked() {
        return Err("plane grant was revoked".into());
    }
    let current = resolve_plane_grant(token, auth)
        .await
        .map_err(|_| "runtime authority is unavailable".to_string())?
        .ok_or_else(|| "plane grant was revoked or expired".to_string())?;
    if grant_authority_key(&current) != grant_authority_key(original) {
        return Err("plane authority changed while awaiting input".into());
    }
    Ok(current)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_interactive(
    body: &Value,
    token: &str,
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
    call: &Arc<CallState>,
    replay: &Arc<Mutex<GrantReplayState>>,
    runtime: Option<&actix_web::web::Data<MagicianV2Api>>,
    auth: Option<&AuthRuntime>,
) -> Value {
    let tool = body
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = body
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or(json!({}));
    if grant.executors.is_none()
        || !is_callable_configured(grant, config, tool)
        || !grant.permits(tool)
        || grant.is_revoked()
    {
        return error("tool is not available on this plane grant");
    }
    let result = match tool {
        "request_user_input" => {
            let Some(question) = arguments
                .get("question")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty() && s.len() <= 16_384)
            else {
                return error(
                    "request_user_input requires a nonempty question of at most 16384 bytes",
                );
            };
            match input_type_from_arguments(&arguments) {
                Ok(input_type) => match call.ask(question, input_type).await {
                    Ok(value) => match revalidate(token, grant, auth).await {
                        Ok(_) if !call.cancelled.is_cancelled() => answered(value),
                        Ok(_) => error("request cancelled"),
                        Err(message) => error(message),
                    },
                    Err(message) => error(message),
                },
                Err(message) => error(message),
            }
        },
        "wait_for_run" => wait_for_run(&arguments, token, grant, call, replay, runtime, auth).await,
        _ => {
            // The route is scoped to this future only. It lets tools already
            // using UserRequestService ask on the same POST stream, with the
            // service retaining its durable first-response-wins ownership.
            let result =
                plane_tools_call_configured(grant, config, Some(&call.session), tool, &arguments)
                    .await;
            if result
                .pointer("/_meta/toolsListChanged")
                .and_then(Value::as_bool)
                == Some(true)
            {
                call.send(&json!({"jsonrpc":"2.0", "method":"notifications/tools/list_changed", "params":{}})).await;
                super::notify_other_sessions(token, Some(&call.session)).await;
            }
            if tool == "run_task" && result.get("isError") == Some(&json!(false)) {
                if let (Some(execution), Some(task)) = (
                    result.get("execution_id").and_then(Value::as_str),
                    result.get("task_id").and_then(Value::as_str),
                ) {
                    replay.lock().await.run_origins.insert(
                        execution.into(),
                        RunOrigin {
                            session: call.session.clone(),
                            task_id: task.into(),
                            waiter: None,
                        },
                    );
                }
            }
            if let Some(capture) = result.pointer("/_meta/planeApprovalCapture") {
                let Ok(capture) = serde_json::from_value::<PlanePendingApproval>(capture.clone())
                else {
                    return error("approval capture could not be reconstructed");
                };
                let question = result
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or("Approve this action?");
                let input_type = UserInputType::Confirmation {
                    confirm_label: None,
                    deny_label: None,
                    destructive: false,
                };
                match call.ask(question, input_type).await {
                    Ok(UserInputValue::Confirmation { confirmed: true }) => {
                        match revalidate(token, grant, auth).await {
                            Ok(mut current) if !call.cancelled.is_cancelled() => {
                                if !current.permits(tool)
                                    || !is_callable_configured(&current, config, tool)
                                {
                                    return error(
                                        "the captured tool is no longer available on this grant",
                                    );
                                }
                                // A fresh projection carries no session parent:
                                // the session's is adopted again, so the approved
                                // dispatch routes as the captured call did.
                                let session =
                                    replay.lock().await.sessions.get(&call.session).cloned();
                                if let Some(session) = session.as_ref() {
                                    super::adopt_session_parent(&mut current, session);
                                }
                                plane_execute_approved_capture(
                                    &current,
                                    &call.session,
                                    tool,
                                    &capture,
                                    config,
                                    &call.cancelled,
                                )
                                .await
                            },
                            Ok(_) => error("request cancelled"),
                            Err(message) => error(message),
                        }
                    },
                    Ok(_) => error("the user declined or cancelled approval"),
                    Err(message) => error(message),
                }
            } else {
                result
            }
        },
    };
    // The ordinary governed dispatch records its own successful calls. Record
    // these plane-owned interactions and any refused continuation without data.
    if matches!(tool, "request_user_input" | "wait_for_run")
        || result.get("isError") == Some(&json!(true))
    {
        magician::magician_v2::execution::plane::record(
            &call.session,
            TerminalLedgerEntry {
                tool: tool.into(),
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                outcome: if result.get("isError") == Some(&json!(true)) {
                    TerminalLedgerOutcome::Refused {
                        reason: "input_not_resumed".into(),
                    }
                } else {
                    TerminalLedgerOutcome::Executed { is_error: false }
                },
            },
        );
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_run(
    arguments: &Value,
    token: &str,
    grant: &PlaneGrant,
    call: &Arc<CallState>,
    replay: &Arc<Mutex<GrantReplayState>>,
    runtime: Option<&actix_web::web::Data<MagicianV2Api>>,
    auth: Option<&AuthRuntime>,
) -> Value {
    let Some(execution_id) = arguments.get("execution_id").and_then(Value::as_str) else {
        return error("wait_for_run requires execution_id");
    };
    let origin = {
        let mut replay = replay.lock().await;
        let Some(origin) = replay
            .run_origins
            .get_mut(execution_id)
            .filter(|origin| origin.session == call.session)
        else {
            return error("this run was not launched by this MCP session");
        };
        if origin.waiter.is_some() {
            return error("this run already has an active wait call");
        }
        origin.waiter = Some(call.request_id.clone());
        origin.clone()
    };
    let _wait_guard = RunWaitGuard {
        replay: replay.clone(),
        execution: execution_id.into(),
        request_id: call.request_id.clone(),
    };
    let (Some(runtime), Some(executors), Some(principal), Some(workspace)) = (
        runtime,
        grant.executors.as_ref(),
        grant.ctx.principal.as_deref(),
        grant.ctx.workspace.as_deref(),
    ) else {
        return error("runtime input continuation is unavailable");
    };
    let Some(service) = executors.artifact_v2_service.as_ref() else {
        return error("execution service is unavailable");
    };
    let timeout_secs = match arguments.get("timeout_secs") {
        None => 300,
        Some(value) => match value.as_u64().filter(|v| (1..=300).contains(v)) {
            Some(v) => v,
            None => return error("timeout_secs must be an integer between 1 and 300"),
        },
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
    // The input continuation this wait handed to the runtime, still running.
    // Dropping the handle when the wait returns detaches it; the run goes on.
    let mut continuation: Option<tokio::task::JoinHandle<Result<u16, String>>> = None;
    'poll: loop {
        if call.cancelled.is_cancelled() {
            return error("wait cancelled; the delegated run retains its current state");
        }
        if continuation
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            let outcome = continuation.take().expect("finished continuation").await;
            if let Some(message) = continuation_refusal(outcome) {
                return error(message);
            }
        }
        if Instant::now() >= deadline {
            return json!({"isError":false, "content":[{"type":"text","text":"Still waiting; call wait_for_run again to continue receiving this run's questions."}], "structuredContent":{"status":"waiting", "execution_id":execution_id}});
        }
        if let Err(message) = revalidate(token, grant, auth).await {
            return error(message);
        }
        let execution = match service
            .get_execution(&scope, &origin.task_id, execution_id)
            .await
        {
            Ok(execution) => execution,
            Err(_) => return error("the delegated execution is unavailable"),
        };
        if matches!(
            execution.state.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            return json!({"isError":false,"content":[{"type":"text","text":format!("Execution {execution_id}: {}", execution.state.status)}],"structuredContent":{"execution_id":execution_id,"status":execution.state.status}});
        }
        let tree = match service.get_execution_tree(&scope, &origin.task_id).await {
            Ok(tree) => tree,
            Err(_) => return error("the delegated execution tree is unavailable"),
        };
        let edges = tree
            .nodes
            .into_iter()
            .map(|node| (node.execution_id, node.parent_execution_id))
            .collect::<Vec<_>>();
        let input_executions =
            match magician::magician_v2::execution::plane::run_ownership::input_execution_ids(
                execution_id,
                &edges,
            ) {
                Ok(ids) => ids,
                Err(message) => return error(message),
            };
        for input_execution in &input_executions {
            // Refresh admission before showing a durable pause; incomplete terminal
            // settlement stays pending just as it does in the existing web endpoint.
            let admission = runtime
                .full_pause_store()
                .refresh_stateless_terminal_pause_admission_scoped(
                    input_execution,
                    None,
                    principal,
                    workspace,
                );
            if admission.is_ok_and(|a| a.staged_count == 0) {
                let pending = runtime
                    .full_pause_store()
                    .get_pending_for_execution(input_execution)
                    .into_iter()
                    .find(|pause| {
                        pause.principal.as_deref() == Some(principal)
                            && pause.workspace.as_deref() == Some(workspace)
                    });
                if let Some(pause) = pending {
                    if pause.input_revision.is_empty() {
                        return error(
                        "this pause needs to be refreshed in Magician before it can be answered",
                    );
                    }
                    // The pause's own spec decides before its typed shape: a
                    // credential ask never reaches this client (plan §6).
                    let input_type = match pause_input_type(&pause) {
                        Ok(input_type) => input_type,
                        Err(message) => return error(message),
                    };
                    let value = match call
                        .ask(
                            pause
                                .question
                                .as_deref()
                                .unwrap_or("Magician needs your input"),
                            input_type,
                        )
                        .await
                    {
                        Ok(value) => value,
                        Err(message) => return error(message),
                    };
                    if let Err(message) = revalidate(token, grant, auth).await {
                        return error(message);
                    }
                    if call.cancelled.is_cancelled() {
                        return error("wait cancelled; input was not submitted");
                    }
                    let value = match serde_json::to_value(value).and_then(serde_json::from_value) {
                        Ok(value) => value,
                        Err(_) => return error("input could not be converted for runtime resume"),
                    };
                    let revision = pause.input_revision.clone();
                    let req = resume_request_for_pause(pause, value);
                    // The resume owner checks scope, exact revision, protected
                    // authority and lifecycle exclusion before consuming the
                    // input — and then answers only when the resumed run next
                    // parks or ends. That is the run's time, not this wait's:
                    // the continuation runs on its own task, a refusal gets
                    // the window below to surface, and the wait returns to
                    // polling the run under its own deadline.
                    let api = runtime.clone();
                    let execution = input_execution.clone();
                    let resume_scope = (principal.to_owned(), workspace.to_owned());
                    let mut handle = actix_web::rt::spawn(async move {
                        api.resume_agentic_execution_from_plane(
                            execution,
                            req,
                            resume_scope,
                            revision,
                        )
                        .await
                        .map(|response| response.status().as_u16())
                        .map_err(|error| error.to_string())
                    });
                    let window = deadline
                        .saturating_duration_since(Instant::now())
                        .min(CONTINUATION_REFUSAL_WINDOW);
                    match tokio::time::timeout(window, &mut handle).await {
                        Ok(outcome) => {
                            if let Some(message) = continuation_refusal(outcome) {
                                return error(message);
                            }
                        },
                        Err(_elapsed) => continuation = Some(handle),
                    }
                    continue 'poll;
                }
            }
        }
        if let Some(requests) = executors.user_request_service.as_ref() {
            let pending = requests
                .list_pending_for_scope(principal, workspace)
                .await
                .into_iter()
                .find(|r| {
                    r.execution_id
                        .as_ref()
                        .is_some_and(|id| input_executions.contains(id))
                });
            if let Some(request) = pending {
                let input_type = match service_input_type(&request) {
                    Ok(t) => t,
                    Err(message) => return error(message),
                };
                let value = match call.ask(&request.question, input_type).await {
                    Ok(v) => v,
                    Err(message) => return error(message),
                };
                if let Err(message) = revalidate(token, grant, auth).await {
                    return error(message);
                }
                if call.cancelled.is_cancelled() {
                    return error("wait cancelled; input was not submitted");
                }
                match requests
                    .respond_scoped(
                        service_response(&request, value),
                        Some(principal),
                        Some(workspace),
                    )
                    .await
                {
                    ScopedResponseResult::Accepted | ScopedResponseResult::AlreadyResolved => {
                        continue
                    },
                    _ => return error("runtime could not accept the input response"),
                }
            }
        }
        tokio::select! {
            _ = call.cancelled.cancelled() => {},
            _ = tokio::time::sleep(Duration::from_millis(500)) => {},
        }
    }
}

/// How long a wait call stays on a freshly handed input continuation before
/// it goes back to polling the run. The resume owner's admission checks answer
/// within this; a run that is simply working is left to it.
const CONTINUATION_REFUSAL_WINDOW: Duration = Duration::from_secs(20);

/// What a finished input continuation means for the wait: nothing when the
/// runtime accepted it, else the refusal with the runtime's status — the
/// status is the one fact that tells a 400 (the request) from a 409 (the
/// pause moved) from a 500 (the resumed run died at its first phase).
fn continuation_refusal(
    outcome: Result<Result<u16, String>, tokio::task::JoinError>,
) -> Option<String> {
    match outcome {
        Ok(Ok(status)) if (200..300).contains(&status) => None,
        Ok(Ok(status)) => Some(format!(
            "runtime refused the input continuation (HTTP {status}); refresh this run's pending question"
        )),
        Ok(Err(error)) => Some(format!("runtime input continuation failed: {error}")),
        Err(join) => Some(format!("runtime input continuation failed: {join}")),
    }
}

/// The resume request for a durable pause the door is answering. The pause is
/// addressed by its exact key; the agent routing trio is a selector of its own
/// that the resume owner takes all-or-nothing, and a run launched for an agent
/// pauses with the agent on its state and no goal or cycle — sent alone, the
/// agent was refused as incomplete routing and the answer never reached the run.
fn resume_request_for_pause(
    pause: magician::magician_v2::execution::agentic::PendingPauseInfo,
    value: crate::web_api::AgenticResumeValue,
) -> AgenticResumeRequest {
    let (agent_id, goal_id, cycle_id) = match (pause.agent_id, pause.goal_id, pause.cycle_id) {
        (Some(agent_id), Some(goal_id), Some(cycle_id)) => {
            (Some(agent_id), Some(goal_id), Some(cycle_id))
        },
        _ => (None, None, None),
    };
    AgenticResumeRequest {
        pause_state_id: Some(pause.key),
        plan_id: None,
        step_id: None,
        input_type: pause.input_type.type_name().into(),
        value,
        agent_id,
        goal_id,
        cycle_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_api::AgenticResumeValue;
    use magician::magician_v2::execution::agentic::PendingPauseInfo;

    /// A run launched for an agent pauses with the agent on its state and no
    /// goal or cycle. The resume owner reads that trio all-or-nothing, so the
    /// door addresses the pause by its exact key and sends the trio only when
    /// it is complete — sent alone, the agent was refused as incomplete
    /// routing and the answered word never reached the run.
    #[test]
    fn the_door_addresses_a_run_pause_by_key_and_never_sends_a_partial_agent_route() {
        let mut pause = PendingPauseInfo::for_test(
            "k2_abc",
            "exec-1",
            UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
        );
        pause.agent_id = Some("personal-assistant".into());

        let request = resume_request_for_pause(
            pause.clone(),
            AgenticResumeValue::Text {
                value: "amber".into(),
            },
        );
        assert_eq!(request.pause_state_id.as_deref(), Some("k2_abc"));
        assert_eq!(request.input_type, "text");
        assert!(
            request.agent_id.is_none() && request.goal_id.is_none() && request.cycle_id.is_none()
        );

        pause.goal_id = Some("goal-1".into());
        pause.cycle_id = Some("cycle-1".into());
        let request = resume_request_for_pause(
            pause,
            AgenticResumeValue::Text {
                value: "amber".into(),
            },
        );
        assert_eq!(request.agent_id.as_deref(), Some("personal-assistant"));
        assert_eq!(request.goal_id.as_deref(), Some("goal-1"));
        assert_eq!(request.cycle_id.as_deref(), Some("cycle-1"));
    }

    /// The resume owner answers only when the resumed run next parks or ends,
    /// so its outcome is read off a task the wait call does not block on. A
    /// success keeps the wait polling; a refusal names the runtime's status,
    /// because "refused" without it hid a 400 for a whole afternoon; a failed
    /// or panicked continuation is reported as one.
    #[test]
    fn a_continuation_outcome_is_reported_with_its_status() {
        assert_eq!(continuation_refusal(Ok(Ok(200))), None);
        assert_eq!(continuation_refusal(Ok(Ok(202))), None);
        assert_eq!(
            continuation_refusal(Ok(Ok(409))).as_deref(),
            Some("runtime refused the input continuation (HTTP 409); refresh this run's pending question")
        );
        assert_eq!(
            continuation_refusal(Ok(Err("boom".to_string()))).as_deref(),
            Some("runtime input continuation failed: boom")
        );
    }
}
