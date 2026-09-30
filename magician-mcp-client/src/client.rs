use std::{
    collections::HashSet,
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams,
        CancelTaskRequest, CancelledNotification, CancelledNotificationParam, ClientInfo,
        ClientRequest, GetTaskParams, GetTaskRequest, Implementation, ListToolsRequest,
        PaginatedRequestParams, ProtocolVersion, RequestId, ResourceUpdatedNotificationParam,
        ServerResult, TaskStatusNotificationParams, UpdateTaskParams, UpdateTaskRequest,
    },
    service::{
        ClientLifecycleMode, ClientServiceExt, NotificationContext, Peer, PeerRequestOptions,
        RoleClient, RunningService, ServiceError,
    },
    ClientHandler,
};
use serde_json::{Map, Value};
use tokio::process::Command;

use crate::{
    config::{McpClientConfig, McpTransportConfig},
    continuation::{McpContinuationOwner, McpTaskPollCommit},
    duplex_json_transport::BoundedDuplexJsonTransport,
    http_transport::build_bounded_http_transport,
    invalidation::{
        protocol_uses_current_subscriptions, subscribe_legacy_resources,
        subscription_channel_capacity, validate_subscription_request, McpInvalidationOwner,
        ValidatedSubscriptionRequest,
    },
    recovery::{McpTaskRecoveryRecord, MAX_RECOVERY_CLOCK_SKEW},
    stdio_transport::BoundedChildProcess,
    validation::{
        drop_json_map_iterative, measure_serialized_request, project_call_response,
        project_connection_info, project_tool_page, validate_call_arguments,
        validate_task_poll_result, ValidatedCallResponse,
    },
    McpCallCancellation, McpClaimedMrtrCall, McpClientError, McpClientLimits, McpConnectionInfo,
    McpInvalidationState, McpMrtrInputSlot, McpMrtrPresentationCapabilities, McpMrtrResponse,
    McpNotificationSubscription, McpPreparedMrtrResponses, McpPreparedTaskRecovery,
    McpPreparedTaskResponses, McpSubscriptionCapabilities, McpSubscriptionRequest,
    McpTaskLifecycleCapabilities, McpTaskPollOutcome, McpTaskProgress, McpTaskRecoveryBinding,
    McpTaskRecoveryCheckpoint, McpToolCallOutcome, McpToolDescriptor, McpToolId,
};

use crate::task::McpTaskNotificationHints;

type RunningMcpService = RunningService<RoleClient, McpClientHandler>;
static NEXT_CLIENT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

struct McpRunningClientConfig {
    limits: McpClientLimits,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    continuation_timeout: Duration,
    mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
    task_lifecycle_capabilities: McpTaskLifecycleCapabilities,
    task_poll_floor: Duration,
    task_hints: Arc<McpTaskNotificationHints>,
    invalidations: Arc<McpInvalidationOwner>,
    subscription_capabilities: McpSubscriptionCapabilities,
    subscription_channel_capacity: usize,
}

#[derive(Clone)]
struct McpClientHandler {
    info: ClientInfo,
    task_hints: Arc<McpTaskNotificationHints>,
    invalidations: Arc<McpInvalidationOwner>,
}

impl ClientHandler for McpClientHandler {
    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.invalidations.mark_tools();
        std::future::ready(())
    }

    fn on_prompt_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.invalidations.mark_prompts();
        std::future::ready(())
    }

    fn on_resource_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.invalidations.mark_resources();
        std::future::ready(())
    }

    fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.invalidations.mark_resource(&params.uri);
        std::future::ready(())
    }

    fn on_task_status(
        &self,
        params: TaskStatusNotificationParams,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.task_hints.mark(&params.task.task.task_id);
        std::future::ready(())
    }

    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }
}

/// One connected, validated MCP server session.
///
/// Tool calls are accepted only for identifiers minted by the latest successful discovery
/// snapshot, preventing arbitrary model-selected method names from bypassing discovery.
pub struct McpClient {
    service: RunningMcpService,
    limits: McpClientLimits,
    request_timeout: std::time::Duration,
    shutdown_timeout: std::time::Duration,
    continuation_timeout: std::time::Duration,
    client_instance_id: u64,
    discovery_generation: u64,
    discovered_tools: HashSet<McpToolId>,
    connection_info: McpConnectionInfo,
    mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
    task_lifecycle_capabilities: McpTaskLifecycleCapabilities,
    task_lifecycle_negotiated: bool,
    continuations: McpContinuationOwner,
    invalidations: Arc<McpInvalidationOwner>,
    subscription_capabilities: McpSubscriptionCapabilities,
    negotiated_subscription_capabilities: McpSubscriptionCapabilities,
    current_subscriptions: bool,
    subscription_channel_capacity: usize,
}

impl McpClient {
    pub async fn connect(config: McpClientConfig) -> Result<Self, McpClientError> {
        config.validate()?;
        let McpClientConfig {
            transport,
            limits,
            connect_timeout,
            request_timeout,
            shutdown_timeout,
            continuation_timeout,
            mrtr_presentation_capabilities,
            task_lifecycle_capabilities,
            task_poll_floor,
            subscription_capabilities,
            subscription_channel_capacity,
        } = config;
        let task_hints = Arc::new(McpTaskNotificationHints::new(
            limits.max_pending_continuations,
        ));
        let invalidations = Arc::new(McpInvalidationOwner::new());

        match transport {
            McpTransportConfig::Stdio(config) => {
                let executable = config.executable.clone();
                let mut command = Command::new(&config.executable);
                command
                    .args(config.args)
                    .env_clear()
                    .envs(config.environment);
                if let Some(working_directory) = config.working_directory {
                    command.current_dir(working_directory);
                }
                let transport =
                    BoundedChildProcess::spawn(command, limits.max_transport_message_bytes)
                        .map_err(|source| McpClientError::Spawn { executable, source })?;
                let service = connect_service(
                    transport,
                    "stdio",
                    connect_timeout,
                    auto_lifecycle(),
                    mrtr_presentation_capabilities,
                    task_lifecycle_capabilities,
                    Arc::clone(&task_hints),
                    Arc::clone(&invalidations),
                )
                .await?;
                Self::from_running(
                    service,
                    "stdio",
                    McpRunningClientConfig {
                        limits,
                        request_timeout,
                        shutdown_timeout,
                        continuation_timeout,
                        mrtr_presentation_capabilities,
                        task_lifecycle_capabilities,
                        task_poll_floor,
                        task_hints,
                        invalidations,
                        subscription_capabilities,
                        subscription_channel_capacity,
                    },
                )
                .await
            },
            McpTransportConfig::StreamableHttp(config) => {
                let transport = build_bounded_http_transport(
                    config.endpoint,
                    config.bearer_token,
                    limits.max_transport_message_bytes,
                    limits.max_sse_event_bytes,
                )
                .map_err(|_| McpClientError::Connect {
                    transport: "streamable_http",
                    message: "HTTP transport initialization failed".to_owned(),
                })?;
                let service = connect_service(
                    transport,
                    "streamable_http",
                    connect_timeout,
                    auto_lifecycle(),
                    mrtr_presentation_capabilities,
                    task_lifecycle_capabilities,
                    Arc::clone(&task_hints),
                    Arc::clone(&invalidations),
                )
                .await?;
                Self::from_running(
                    service,
                    "streamable_http",
                    McpRunningClientConfig {
                        limits,
                        request_timeout,
                        shutdown_timeout,
                        continuation_timeout,
                        mrtr_presentation_capabilities,
                        task_lifecycle_capabilities,
                        task_poll_floor,
                        task_hints,
                        invalidations,
                        subscription_capabilities,
                        subscription_channel_capacity,
                    },
                )
                .await
            },
            McpTransportConfig::DuplexJson(config) => {
                let transport =
                    BoundedDuplexJsonTransport::new(config, limits.max_transport_message_bytes);
                let service = connect_service(
                    transport,
                    "duplex_json",
                    connect_timeout,
                    ClientLifecycleMode::Discover {
                        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    },
                    mrtr_presentation_capabilities,
                    task_lifecycle_capabilities,
                    Arc::clone(&task_hints),
                    Arc::clone(&invalidations),
                )
                .await?;
                Self::from_running(
                    service,
                    "duplex_json",
                    McpRunningClientConfig {
                        limits,
                        request_timeout,
                        shutdown_timeout,
                        continuation_timeout,
                        mrtr_presentation_capabilities,
                        task_lifecycle_capabilities,
                        task_poll_floor,
                        task_hints,
                        invalidations,
                        subscription_capabilities,
                        subscription_channel_capacity,
                    },
                )
                .await
            },
        }
    }

    pub fn connection_info(&self) -> &McpConnectionInfo {
        &self.connection_info
    }

    /// Exact governed MRTR input kinds advertised for this immutable connection.
    pub fn mrtr_presentation_capabilities(&self) -> McpMrtrPresentationCapabilities {
        self.mrtr_presentation_capabilities
    }

    /// Exact task support configured for this immutable connection.
    pub fn task_lifecycle_capabilities(&self) -> McpTaskLifecycleCapabilities {
        self.task_lifecycle_capabilities
    }

    /// Whether both peers advertised the official Tasks extension.
    pub fn task_lifecycle_negotiated(&self) -> bool {
        self.task_lifecycle_negotiated
    }

    /// Exact notification categories installed by the trusted caller.
    pub fn subscription_capabilities(&self) -> McpSubscriptionCapabilities {
        self.subscription_capabilities
    }

    /// Installed categories that the initialized server also advertised.
    pub fn negotiated_subscription_capabilities(&self) -> McpSubscriptionCapabilities {
        self.negotiated_subscription_capabilities
    }

    /// Return the payload-free invalidation state for this connection.
    pub fn invalidation_state(&self) -> Result<McpInvalidationState, McpClientError> {
        self.invalidations.state()
    }

    /// Open one exact, bounded official notification subscription.
    ///
    /// The default client configuration installs no categories. The server must
    /// acknowledge every requested category and resource URI exactly before this
    /// method returns. At most one subscription can be active per client.
    pub async fn open_notification_subscription(
        &self,
        request: McpSubscriptionRequest,
    ) -> Result<McpNotificationSubscription, McpClientError> {
        let validated = validate_subscription_request(
            request,
            self.subscription_capabilities,
            self.negotiated_subscription_capabilities,
            &self.limits,
        )?;
        self.open_notification_subscription_inner(validated).await
    }

    async fn open_notification_subscription_inner(
        &self,
        validated: ValidatedSubscriptionRequest,
    ) -> Result<McpNotificationSubscription, McpClientError> {
        let accepted = validated.capabilities;
        let resource_uris = validated.resource_uris.clone();
        let id = self.invalidations.begin(accepted, resource_uris.clone())?;

        if self.current_subscriptions {
            let capacity = subscription_channel_capacity(self.subscription_channel_capacity)?;
            let subscription = match tokio::time::timeout(
                self.request_timeout,
                self.service
                    .peer()
                    .listen_with_capacity(validated.sdk_filter(), capacity),
            )
            .await
            {
                Err(_) => {
                    self.invalidations.release(id);
                    return Err(McpClientError::SubscriptionTimeout);
                },
                Ok(Err(_)) => {
                    self.invalidations.release(id);
                    return Err(McpClientError::SubscriptionFailed);
                },
                Ok(Ok(subscription)) => subscription,
            };
            if !validated.exactly_acknowledged(subscription.acknowledged()) {
                let mut subscription = subscription;
                let _ = tokio::time::timeout(self.request_timeout, subscription.cancel()).await;
                self.invalidations.release(id);
                return Err(McpClientError::SubscriptionProtocolViolation);
            }
            let observed_revision = match self.invalidations.activate(id) {
                Ok(revision) => revision,
                Err(error) => {
                    let mut subscription = subscription;
                    let _ = tokio::time::timeout(self.request_timeout, subscription.cancel()).await;
                    self.invalidations.release(id);
                    return Err(error);
                },
            };
            return Ok(McpNotificationSubscription::current(
                subscription,
                Arc::clone(&self.invalidations),
                id,
                accepted,
                observed_revision,
                self.request_timeout,
            ));
        }

        if let Err(error) =
            subscribe_legacy_resources(self.service.peer(), &resource_uris, self.request_timeout)
                .await
        {
            self.invalidations.release(id);
            return Err(error);
        }
        let observed_revision = match self.invalidations.activate(id) {
            Ok(revision) => revision,
            Err(error) => {
                self.invalidations.release(id);
                return Err(error);
            },
        };
        Ok(McpNotificationSubscription::legacy(
            self.service.peer().clone(),
            resource_uris.into_iter().collect(),
            Arc::clone(&self.invalidations),
            id,
            accepted,
            observed_revision,
            self.request_timeout,
        ))
    }

    /// Whether this exact client still owns the unexpired continuation revision.
    pub fn pending_call_is_active(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<bool, McpClientError> {
        self.continuations.is_active(pending)
    }

    /// Number of incomplete MRTR/task continuations retained by this client.
    pub fn pending_call_count(&self) -> Result<usize, McpClientError> {
        self.continuations.pending_count()
    }

    /// Classify whether an exact active continuation can survive SDK-session loss.
    pub fn continuation_recovery_disposition(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<crate::McpContinuationRecoveryDisposition, McpClientError> {
        self.continuations.recovery_disposition(pending)
    }

    /// Mint a minimal restart checkpoint for one exact active remote task.
    ///
    /// MRTR and ordinary in-flight state fail closed because their SDK request authority
    /// cannot be reconstructed safely. The product must durably replace this checkpoint
    /// after every successful or ambiguous task operation and atomically claim it before
    /// permitting recovered update/cancel side effects.
    pub fn checkpoint_task(
        &self,
        binding: McpTaskRecoveryBinding,
        pending: crate::McpPendingCall,
    ) -> Result<McpTaskRecoveryCheckpoint, McpClientError> {
        self.require_tasks_negotiated()?;
        let snapshot = self.continuations.task_recovery_snapshot(pending)?;
        let record = McpTaskRecoveryRecord::new(
            binding,
            self.connection_info.transport,
            &self.connection_info.protocol_version,
            self.connection_info.server_name.as_deref(),
            self.connection_info.server_version.as_deref(),
            &snapshot.tool_name,
            &snapshot.task_id,
            &snapshot.created_at,
            snapshot.started_at_epoch_millis,
            snapshot.lifetime_expires_at_epoch_millis,
            snapshot.expires_at_epoch_millis,
            snapshot.operation_count,
            snapshot.cancel_requested,
            snapshot.revision,
        )?;
        McpTaskRecoveryCheckpoint::encode(&record)
    }

    /// Prepare a restart attempt and consume its durable operation revision.
    ///
    /// This performs no network I/O. The returned replacement checkpoint must be
    /// durably committed before dispatch so timeout, transport loss, or process death
    /// cannot make the same operation-budget revision replayable.
    pub fn prepare_task_recovery(
        &self,
        binding: McpTaskRecoveryBinding,
        checkpoint: &McpTaskRecoveryCheckpoint,
        tool: &McpToolId,
    ) -> Result<McpPreparedTaskRecovery, McpClientError> {
        self.require_tasks_negotiated()?;
        let mut record = checkpoint.decode(binding)?;
        self.validate_task_recovery_record(&record, tool)?;
        record.operation_count = record
            .operation_count
            .checked_add(1)
            .ok_or(McpClientError::TaskOperationLimitExceeded)?;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        McpPreparedTaskRecovery::new(record, tool.clone())
    }

    /// Reconnect one prepared checkpoint through authoritative official `tasks/get`.
    ///
    /// Only a validated nonterminal response mints a fresh process-local pending handle.
    /// A failed request leaves no local authority; another attempt must be prepared from
    /// the replacement checkpoint that was persisted before this dispatch.
    pub async fn recover_task(
        &self,
        prepared: McpPreparedTaskRecovery,
    ) -> Result<McpTaskPollOutcome, McpClientError> {
        self.require_tasks_negotiated()?;
        let McpPreparedTaskRecovery { record, tool, .. } = prepared;
        let request = ClientRequest::GetTaskRequest(GetTaskRequest::new(GetTaskParams::new(
            record.task_id.clone(),
        )));
        let response =
            send_request_with_timeout(self.service.peer(), request, self.request_timeout)
                .await
                .map_err(map_task_error)?;
        let ServerResult::GetTaskResult(result) = response else {
            return Err(McpClientError::Call(
                "server returned an unexpected task recovery response".to_owned(),
            ));
        };
        let payload_bytes =
            validate_task_poll_result(&result, &self.limits, self.mrtr_presentation_capabilities)?;
        let commit = self
            .continuations
            .recover_task(record, tool, result, payload_bytes)?;
        self.project_task_poll_commit(commit)
    }

    /// Return payload-free state and governed polling delay for one exact task revision.
    pub fn pending_task_progress(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<McpTaskProgress, McpClientError> {
        self.require_tasks_negotiated()?;
        self.continuations.task_progress(pending)
    }

    /// Return payload-free response slots for a task waiting on client input.
    pub fn pending_task_input_slots(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
        self.require_tasks_negotiated()?;
        self.continuations.task_input_slots(pending)
    }

    /// Validate and privately bind a complete response set for one task revision.
    pub fn prepare_task_responses(
        &self,
        pending: crate::McpPendingCall,
        responses: Vec<McpMrtrResponse>,
    ) -> Result<McpPreparedTaskResponses, McpClientError> {
        self.require_tasks_negotiated()?;
        self.continuations
            .prepare_task_responses(pending, responses)
    }

    /// Return payload-free response slots for one exact active MRTR revision.
    pub fn pending_input_slots(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
        self.continuations.input_slots(pending)
    }

    /// Validate and bind a complete product response set without claiming or resuming it.
    ///
    /// The returned object is move-only and remains non-authorizing. A later resume path
    /// must atomically claim and revalidate the exact continuation revision.
    pub fn prepare_mrtr_responses(
        &self,
        pending: crate::McpPendingCall,
        responses: Vec<McpMrtrResponse>,
    ) -> Result<McpPreparedMrtrResponses, McpClientError> {
        self.continuations
            .prepare_input_responses(pending, responses)
    }

    /// Atomically lease one prepared MRTR response bundle for a future SDK retry.
    ///
    /// This rechecks the exact owner, kind, revision, and monotonic deadline while
    /// holding the continuation lock. No network request is sent by this method.
    /// Dropping the returned move-only lease restores the same active revision.
    pub fn claim_mrtr_responses(
        &self,
        prepared: McpPreparedMrtrResponses,
    ) -> Result<McpClaimedMrtrCall, McpClientError> {
        self.continuations.claim_input_responses(prepared)
    }

    /// Retry one atomically claimed MRTR round through the official SDK.
    ///
    /// This consumes the claim, echoes the SDK-owned request state unchanged, and
    /// returns either a validated terminal result or a new exact continuation revision.
    /// The immutable presentation capability set is enforced on every replacement
    /// round. Automatic multi-round driving remains disabled.
    pub async fn resume_mrtr(
        &self,
        claim: McpClaimedMrtrCall,
    ) -> Result<McpToolCallOutcome, McpClientError> {
        self.resume_mrtr_inner(claim, None).await
    }

    /// Retry one claimed MRTR round with explicit caller cancellation.
    ///
    /// An already-cancelled token stops before transport and restores the claimed
    /// revision. Cancellation after dispatch sends an exact SDK cancellation request,
    /// consumes the dispatched revision, and releases its local reservation.
    pub async fn resume_mrtr_with_cancellation(
        &self,
        claim: McpClaimedMrtrCall,
        cancellation: &McpCallCancellation,
    ) -> Result<McpToolCallOutcome, McpClientError> {
        self.resume_mrtr_inner(claim, Some(cancellation)).await
    }

    async fn resume_mrtr_inner(
        &self,
        mut claim: McpClaimedMrtrCall,
        cancellation: Option<&McpCallCancellation>,
    ) -> Result<McpToolCallOutcome, McpClientError> {
        if cancellation.is_some_and(McpCallCancellation::is_cancelled) {
            return Err(McpClientError::CallCancelled);
        }
        let (retry, params) = self.continuations.begin_input_retry(&mut claim)?;
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let response = send_request_with_control(
            self.service.peer(),
            request,
            self.request_timeout,
            cancellation,
        )
        .await
        .map_err(|error| match error {
            ControlledRequestError::Service(error) => map_call_error(error),
            ControlledRequestError::Cancelled => McpClientError::CallCancelled,
        })?;
        let response = call_response_from_server_result(response)?;
        match project_call_response(
            response,
            &self.limits,
            self.mrtr_presentation_capabilities,
            self.task_lifecycle_negotiated,
        )? {
            ValidatedCallResponse::Complete(result) => {
                retry.complete();
                Ok(McpToolCallOutcome::Complete(result))
            },
            ValidatedCallResponse::Pending(pending) => {
                let kind = pending.kind();
                let pending = retry.commit(pending)?;
                match kind {
                    crate::McpContinuationKind::AdditionalInput => {
                        Ok(McpToolCallOutcome::InputRequired(pending))
                    },
                    crate::McpContinuationKind::RemoteTask => Ok(McpToolCallOutcome::Task(pending)),
                }
            },
        }
    }

    /// Perform one governed authoritative `tasks/get` poll.
    pub async fn poll_task(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<McpTaskPollOutcome, McpClientError> {
        self.poll_task_inner(pending, None).await
    }

    /// Perform one governed task poll with explicit caller cancellation.
    pub async fn poll_task_with_cancellation(
        &self,
        pending: crate::McpPendingCall,
        cancellation: &McpCallCancellation,
    ) -> Result<McpTaskPollOutcome, McpClientError> {
        self.poll_task_inner(pending, Some(cancellation)).await
    }

    async fn poll_task_inner(
        &self,
        pending: crate::McpPendingCall,
        cancellation: Option<&McpCallCancellation>,
    ) -> Result<McpTaskPollOutcome, McpClientError> {
        self.require_tasks_negotiated()?;
        if cancellation.is_some_and(McpCallCancellation::is_cancelled) {
            return Err(McpClientError::CallCancelled);
        }
        let (mut claim, task_id) = self.continuations.begin_task_poll(pending)?;
        let request =
            ClientRequest::GetTaskRequest(GetTaskRequest::new(GetTaskParams::new(task_id)));
        claim.mark_dispatched();
        let response = send_request_with_control(
            self.service.peer(),
            request,
            self.request_timeout,
            cancellation,
        )
        .await
        .map_err(map_controlled_task_error)?;
        let ServerResult::GetTaskResult(result) = response else {
            return Err(McpClientError::Call(
                "server returned an unexpected task response".to_owned(),
            ));
        };
        let payload_bytes =
            validate_task_poll_result(&result, &self.limits, self.mrtr_presentation_capabilities)?;
        let commit = self
            .continuations
            .commit_task_poll(claim, result, payload_bytes)?;
        self.project_task_poll_commit(commit)
    }

    fn project_task_poll_commit(
        &self,
        commit: McpTaskPollCommit,
    ) -> Result<McpTaskPollOutcome, McpClientError> {
        match commit {
            McpTaskPollCommit::Pending(pending) => Ok(McpTaskPollOutcome::Pending(
                self.continuations.task_progress(pending)?,
            )),
            McpTaskPollCommit::Terminal(payload) => match payload {
                rmcp::model::TaskPayload::Completed { result } => {
                    let result: CallToolResult = serde_json::from_value(Value::Object(result))
                        .map_err(|_| {
                            McpClientError::ResponseRejected(
                                "completed task returned an invalid tool result".to_owned(),
                            )
                        })?;
                    match project_call_response(
                        CallToolResponse::Complete(result),
                        &self.limits,
                        self.mrtr_presentation_capabilities,
                        self.task_lifecycle_negotiated,
                    )? {
                        ValidatedCallResponse::Complete(result) => {
                            Ok(McpTaskPollOutcome::Complete(result))
                        },
                        ValidatedCallResponse::Pending(_) => Err(McpClientError::ResponseRejected(
                            "completed task returned an incomplete result".to_owned(),
                        )),
                    }
                },
                rmcp::model::TaskPayload::Failed { error } => {
                    crate::validation::drop_json_map_iterative(error);
                    Ok(McpTaskPollOutcome::Failed)
                },
                rmcp::model::TaskPayload::Cancelled => Ok(McpTaskPollOutcome::Cancelled),
                _ => Err(McpClientError::ResponseRejected(
                    "terminal task returned a non-terminal payload".to_owned(),
                )),
            },
        }
    }

    /// Deliver one complete response set with official `tasks/update`.
    ///
    /// The acknowledgement is not treated as authoritative task state. The returned
    /// revision remains retained until a later validated `tasks/get` is terminal.
    pub async fn update_task(
        &self,
        prepared: McpPreparedTaskResponses,
    ) -> Result<crate::McpPendingCall, McpClientError> {
        self.require_tasks_negotiated()?;
        let (mut claim, task_id, responses) = self.continuations.begin_task_update(prepared)?;
        let request = ClientRequest::UpdateTaskRequest(UpdateTaskRequest::new(
            UpdateTaskParams::new(task_id, responses),
        ));
        claim.mark_dispatched();
        let response =
            send_request_with_timeout(self.service.peer(), request, self.request_timeout)
                .await
                .map_err(map_task_error)?;
        if !matches!(
            response,
            ServerResult::TaskAckResult(_) | ServerResult::EmptyResult(_)
        ) {
            return Err(McpClientError::Call(
                "server returned an unexpected task acknowledgement".to_owned(),
            ));
        }
        self.continuations.commit_task_update(claim)
    }

    /// Signal cooperative cancellation through official `tasks/cancel`.
    ///
    /// The task remains retained after the acknowledgement; only a validated terminal
    /// `tasks/get` result releases it.
    pub async fn cancel_task(
        &self,
        pending: crate::McpPendingCall,
    ) -> Result<crate::McpPendingCall, McpClientError> {
        self.require_tasks_negotiated()?;
        let (mut claim, task_id) = self.continuations.begin_task_cancel(pending)?;
        let request = ClientRequest::CancelTaskRequest(CancelTaskRequest::new(
            CancelTaskParams::new(task_id),
        ));
        claim.mark_dispatched();
        let response =
            send_request_with_timeout(self.service.peer(), request, self.request_timeout)
                .await
                .map_err(map_task_error)?;
        if !matches!(
            response,
            ServerResult::TaskAckResult(_) | ServerResult::EmptyResult(_)
        ) {
            return Err(McpClientError::Call(
                "server returned an unexpected task acknowledgement".to_owned(),
            ));
        }
        self.continuations.commit_task_cancel(claim)
    }

    fn validate_task_recovery_record(
        &self,
        record: &McpTaskRecoveryRecord,
        tool: &McpToolId,
    ) -> Result<(), McpClientError> {
        if !self.discovered_tools.contains(tool)
            || record.tool_name != tool.remote_name()
            || record.tool_name.is_empty()
            || record.tool_name.len() > self.limits.max_tool_name_bytes
            || record.tool_name.chars().any(char::is_control)
            || record.transport != self.connection_info.transport
            || record.protocol_version != self.connection_info.protocol_version
            || record.server_name != self.connection_info.server_name
            || record.server_version != self.connection_info.server_version
        {
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        if record.task_id.is_empty()
            || record.task_id.len() > self.limits.max_task_id_bytes
            || record.task_id.chars().any(char::is_control)
            || record.created_at.is_empty()
            || record.created_at.len() > 128
            || record.created_at.chars().any(char::is_control)
            || record.revision == 0
            || record.revision == u64::MAX
        {
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        if record.operation_count
            >= u64::try_from(self.limits.max_task_operations).unwrap_or(u64::MAX)
        {
            return Err(McpClientError::TaskOperationLimitExceeded);
        }
        let now = current_epoch_millis()?;
        let skew = u64::try_from(MAX_RECOVERY_CLOCK_SKEW.as_millis()).unwrap_or(u64::MAX);
        let max_lifetime = u64::try_from(self.continuation_timeout.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(skew);
        if record.started_at_epoch_millis > now.saturating_add(skew)
            || record.expires_at_epoch_millis <= now
            || record.expires_at_epoch_millis > record.lifetime_expires_at_epoch_millis
            || record
                .lifetime_expires_at_epoch_millis
                .saturating_sub(record.started_at_epoch_millis)
                > max_lifetime
            || record.lifetime_expires_at_epoch_millis.saturating_sub(now) > max_lifetime
        {
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        Ok(())
    }

    fn require_tasks_negotiated(&self) -> Result<(), McpClientError> {
        if self.task_lifecycle_negotiated {
            Ok(())
        } else {
            Err(McpClientError::ContinuationCapabilityUnsupported)
        }
    }

    /// Refresh and atomically replace the set of callable remote tools.
    pub async fn discover_tools(&mut self) -> Result<Vec<McpToolDescriptor>, McpClientError> {
        let invalidation_ticket = self.invalidations.tools_ticket()?;
        self.discover_tools_with_ticket(invalidation_ticket).await
    }

    /// Refresh the authoritative tool snapshot only when a subscribed notification
    /// invalidated it. Notifications are hints: a failed refresh preserves both the
    /// prior callable catalog and the dirty revision.
    pub async fn refresh_tools_if_invalidated(
        &mut self,
    ) -> Result<Option<Vec<McpToolDescriptor>>, McpClientError> {
        let Some(invalidation_ticket) = self.invalidations.tools_ticket()? else {
            return Ok(None);
        };
        self.discover_tools_with_ticket(Some(invalidation_ticket))
            .await
            .map(Some)
    }

    async fn discover_tools_with_ticket(
        &mut self,
        invalidation_ticket: Option<u64>,
    ) -> Result<Vec<McpToolDescriptor>, McpClientError> {
        let candidate_generation = self.discovery_generation.checked_add(1).ok_or_else(|| {
            McpClientError::CatalogRejected("tool discovery generation is exhausted".to_owned())
        })?;
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut projected = Vec::new();
        let mut names = HashSet::new();
        let mut catalog_bytes = 0usize;
        let mut page_count = 0usize;

        loop {
            if page_count >= self.limits.max_tool_pages {
                return Err(McpClientError::CatalogRejected(format!(
                    "tool discovery exceeded {} pages",
                    self.limits.max_tool_pages
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(McpClientError::DiscoveryTimeout);
            }
            let request = ClientRequest::ListToolsRequest(ListToolsRequest::with_param(
                PaginatedRequestParams::default().with_cursor(cursor.clone()),
            ));
            let response = send_request_with_timeout(self.service.peer(), request, remaining)
                .await
                .map_err(map_discovery_error)?;
            let ServerResult::ListToolsResult(page) = response else {
                return Err(McpClientError::Discovery(
                    "server returned an unexpected discovery response".to_owned(),
                ));
            };
            page_count = page_count.saturating_add(1);
            project_tool_page(
                page.tools,
                &self.limits,
                &mut projected,
                &mut names,
                &mut catalog_bytes,
                self.client_instance_id,
                candidate_generation,
            )?;

            let Some(next_cursor) = page.next_cursor else {
                break;
            };
            validate_cursor(&next_cursor, &self.limits)?;
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(McpClientError::CatalogRejected(
                    "tool discovery returned a repeated cursor".to_owned(),
                ));
            }
            cursor = Some(next_cursor);
        }

        if let Some(ticket) = invalidation_ticket {
            self.invalidations.acknowledge_tools(ticket)?;
        }
        self.discovered_tools = projected
            .iter()
            .map(|descriptor| descriptor.id.clone())
            .collect();
        self.discovery_generation = candidate_generation;
        Ok(projected)
    }

    /// Execute one tool request without automatically answering MRTR input requests.
    ///
    /// `InputRequired` and task results are returned to the orchestration layer, which can apply
    /// product HITL and authorization before resuming them.
    pub async fn call_tool(
        &self,
        tool: &McpToolId,
        arguments: Map<String, Value>,
    ) -> Result<McpToolCallOutcome, McpClientError> {
        if !self.discovered_tools.contains(tool) {
            drop_json_map_iterative(arguments);
            return Err(McpClientError::ToolNotDiscovered);
        }
        match validate_call_arguments(&arguments, &self.limits) {
            Ok(_) => {},
            Err(error) => {
                drop_json_map_iterative(arguments);
                return Err(error);
            },
        }
        let params =
            CallToolRequestParams::new(tool.remote_name().to_owned()).with_arguments(arguments);
        let retained_request_bytes =
            measure_serialized_request(&params, self.limits.max_request_bytes)?;
        let reservation =
            self.continuations
                .reserve(tool.clone(), params.clone(), retained_request_bytes)?;
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let response =
            send_request_with_timeout(self.service.peer(), request, self.request_timeout)
                .await
                .map_err(map_call_error)?;
        let response = call_response_from_server_result(response)?;
        match project_call_response(
            response,
            &self.limits,
            self.mrtr_presentation_capabilities,
            self.task_lifecycle_negotiated,
        )? {
            ValidatedCallResponse::Complete(result) => {
                reservation.complete();
                Ok(McpToolCallOutcome::Complete(result))
            },
            ValidatedCallResponse::Pending(pending) => {
                let kind = pending.kind();
                let pending = reservation.commit(pending)?;
                match kind {
                    crate::McpContinuationKind::AdditionalInput => {
                        Ok(McpToolCallOutcome::InputRequired(pending))
                    },
                    crate::McpContinuationKind::RemoteTask => Ok(McpToolCallOutcome::Task(pending)),
                }
            },
        }
    }

    pub async fn close(mut self) -> Result<(), McpClientError> {
        let outcome = self
            .service
            .close_with_timeout(self.shutdown_timeout)
            .await
            .map_err(|_| McpClientError::Shutdown("connection cleanup task failed".to_owned()))?;
        if outcome.is_none() {
            return Err(McpClientError::Shutdown(
                "connection cleanup exceeded the configured timeout".to_owned(),
            ));
        }
        Ok(())
    }

    async fn from_running(
        service: RunningMcpService,
        transport: &'static str,
        config: McpRunningClientConfig,
    ) -> Result<Self, McpClientError> {
        Self::from_running_with_budget(service, transport, config, true).await
    }

    #[cfg(test)]
    async fn from_running_unbudgeted(
        service: RunningMcpService,
        transport: &'static str,
        config: McpRunningClientConfig,
    ) -> Result<Self, McpClientError> {
        Self::from_running_with_budget(service, transport, config, false).await
    }

    async fn from_running_with_budget(
        mut service: RunningMcpService,
        transport: &'static str,
        config: McpRunningClientConfig,
        reserve_process_budget: bool,
    ) -> Result<Self, McpClientError> {
        let McpRunningClientConfig {
            limits,
            request_timeout,
            shutdown_timeout,
            continuation_timeout,
            mrtr_presentation_capabilities,
            task_lifecycle_capabilities,
            task_poll_floor,
            task_hints,
            invalidations,
            subscription_capabilities,
            subscription_channel_capacity,
        } = config;
        let Some(peer_info) = service.peer().peer_info() else {
            let _ = service.close_with_timeout(shutdown_timeout).await;
            return Err(McpClientError::MissingPeerInfo);
        };
        let task_lifecycle_negotiated =
            task_lifecycle_capabilities.enabled() && peer_info.capabilities.supports_tasks();
        let negotiated_subscription_capabilities =
            subscription_capabilities.supported_by(&peer_info.capabilities);
        let current_subscriptions =
            protocol_uses_current_subscriptions(&peer_info.protocol_version);
        let connection_info = match project_connection_info(&peer_info, &limits, transport) {
            Ok(info) => info,
            Err(error) => {
                let _ = service.close_with_timeout(shutdown_timeout).await;
                return Err(error);
            },
        };
        let client_instance_id = match allocate_client_instance_id() {
            Ok(instance_id) => instance_id,
            Err(error) => {
                let _ = service.close_with_timeout(shutdown_timeout).await;
                return Err(error);
            },
        };
        tracing::info!(
            transport,
            protocol_version = %connection_info.protocol_version,
            sdk_version = connection_info.sdk_version,
            "MCP client connected"
        );
        #[cfg(test)]
        let continuations = if reserve_process_budget {
            McpContinuationOwner::try_new_with_task_support(
                client_instance_id,
                &limits,
                continuation_timeout,
                task_poll_floor,
                task_hints,
            )
        } else {
            Ok(McpContinuationOwner::new_unbudgeted_with_task_support(
                client_instance_id,
                &limits,
                continuation_timeout,
                task_poll_floor,
                task_hints,
            ))
        };
        #[cfg(not(test))]
        let continuations = {
            let _ = reserve_process_budget;
            McpContinuationOwner::try_new_with_task_support(
                client_instance_id,
                &limits,
                continuation_timeout,
                task_poll_floor,
                task_hints,
            )
        };
        let continuations = match continuations {
            Ok(continuations) => continuations,
            Err(error) => {
                let _ = service.close_with_timeout(shutdown_timeout).await;
                return Err(error);
            },
        };
        Ok(Self {
            service,
            limits,
            request_timeout,
            shutdown_timeout,
            continuation_timeout,
            client_instance_id,
            discovery_generation: 0,
            discovered_tools: HashSet::new(),
            connection_info,
            mrtr_presentation_capabilities,
            task_lifecycle_capabilities,
            task_lifecycle_negotiated,
            continuations,
            invalidations,
            subscription_capabilities,
            negotiated_subscription_capabilities,
            current_subscriptions,
            subscription_channel_capacity,
        })
    }
}

fn allocate_client_instance_id() -> Result<u64, McpClientError> {
    NEXT_CLIENT_INSTANCE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| McpClientError::ClientIdentityExhausted)
}

fn current_epoch_millis() -> Result<u64, McpClientError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
    u64::try_from(duration.as_millis()).map_err(|_| McpClientError::TaskRecoveryRecordRejected)
}

async fn connect_service<T, E, A>(
    transport: T,
    transport_name: &'static str,
    timeout: std::time::Duration,
    lifecycle: ClientLifecycleMode,
    mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
    task_lifecycle_capabilities: McpTaskLifecycleCapabilities,
    task_hints: Arc<McpTaskNotificationHints>,
    invalidations: Arc<McpInvalidationOwner>,
) -> Result<RunningMcpService, McpClientError>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut capabilities = mrtr_presentation_capabilities.sdk_client_capabilities();
    task_lifecycle_capabilities.apply_to(&mut capabilities);
    let client_info = ClientInfo::new(
        capabilities,
        Implementation::new("magician-mcp-client", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(ProtocolVersion::V_2025_11_25);
    let handler = McpClientHandler {
        info: client_info,
        task_hints,
        invalidations,
    };
    tokio::time::timeout(timeout, handler.serve_with_lifecycle(transport, lifecycle))
        .await
        .map_err(|_| McpClientError::ConnectTimeout {
            transport: transport_name,
        })?
        .map_err(|error| McpClientError::Connect {
            transport: transport_name,
            message: stable_initialize_error(&error),
        })
}

fn auto_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

async fn send_request_with_timeout(
    peer: &Peer<RoleClient>,
    request: ClientRequest,
    timeout: Duration,
) -> Result<ServerResult, ServiceError> {
    match send_request_with_control(peer, request, timeout, None).await {
        Ok(response) => Ok(response),
        Err(ControlledRequestError::Service(error)) => Err(error),
        Err(ControlledRequestError::Cancelled) => Err(ServiceError::Cancelled {
            reason: Some("request cancelled".to_owned()),
        }),
    }
}

enum ControlledRequestError {
    Service(ServiceError),
    Cancelled,
}

async fn send_request_with_control(
    peer: &Peer<RoleClient>,
    request: ClientRequest,
    timeout: Duration,
    explicit_cancellation: Option<&McpCallCancellation>,
) -> Result<ServerResult, ControlledRequestError> {
    if explicit_cancellation.is_some_and(McpCallCancellation::is_cancelled) {
        return Err(ControlledRequestError::Cancelled);
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut handle = tokio::time::timeout(
        timeout,
        peer.send_cancellable_request(request, PeerRequestOptions::no_options()),
    )
    .await
    .map_err(|_| ControlledRequestError::Service(ServiceError::Timeout { timeout }))?
    .map_err(ControlledRequestError::Service)?;
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        let _ = handle.cancel(Some("request timeout".to_owned())).await;
        return Err(ControlledRequestError::Service(ServiceError::Timeout {
            timeout,
        }));
    }
    if explicit_cancellation.is_some_and(McpCallCancellation::is_cancelled) {
        let _ = handle
            .cancel(Some("request explicitly cancelled".to_owned()))
            .await;
        return Err(ControlledRequestError::Cancelled);
    }
    handle.options = PeerRequestOptions::with_timeout(remaining);
    let mut drop_cancellation =
        PendingRequestCancellation::new(handle.peer.clone(), handle.id.clone());
    let response = if let Some(token) = explicit_cancellation {
        let mut pending_response = Box::pin(handle.await_response());
        tokio::select! {
            biased;
            response = &mut pending_response => response.map_err(ControlledRequestError::Service),
            () = token.cancelled() => {
                drop(pending_response);
                drop_cancellation.cancel("request explicitly cancelled").await;
                Err(ControlledRequestError::Cancelled)
            },
        }
    } else {
        handle
            .await_response()
            .await
            .map_err(ControlledRequestError::Service)
    };
    drop_cancellation.disarm();
    response
}

fn call_response_from_server_result(
    response: ServerResult,
) -> Result<CallToolResponse, McpClientError> {
    match response {
        ServerResult::CallToolResult(result) => Ok(CallToolResponse::Complete(result)),
        ServerResult::InputRequiredResult(result) => Ok(CallToolResponse::InputRequired(result)),
        ServerResult::CreateTaskResult(result) => Ok(CallToolResponse::Task(result)),
        _ => Err(McpClientError::Call(
            "server returned an unexpected tool response".to_owned(),
        )),
    }
}

struct PendingRequestCancellation {
    pending: Option<(Peer<RoleClient>, RequestId)>,
}

impl PendingRequestCancellation {
    fn new(peer: Peer<RoleClient>, request_id: RequestId) -> Self {
        Self {
            pending: Some((peer, request_id)),
        }
    }

    fn disarm(&mut self) {
        self.pending = None;
    }

    async fn cancel(&mut self, reason: &'static str) {
        let Some((peer, request_id)) = self.pending.as_ref().cloned() else {
            return;
        };
        let notification = CancelledNotification::new(CancelledNotificationParam::new(
            Some(request_id),
            Some(reason.to_owned()),
        ));
        let _ = peer.send_notification(notification.into()).await;
        self.pending = None;
    }
}

impl Drop for PendingRequestCancellation {
    fn drop(&mut self) {
        let Some((peer, request_id)) = self.pending.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let notification = CancelledNotification::new(CancelledNotificationParam::new(
                Some(request_id),
                Some("request future dropped".to_owned()),
            ));
            let _ = peer.send_notification(notification.into()).await;
        });
    }
}

fn validate_cursor(cursor: &str, limits: &McpClientLimits) -> Result<(), McpClientError> {
    if cursor.is_empty()
        || cursor.len() > limits.max_cursor_bytes
        || cursor.chars().any(char::is_control)
    {
        return Err(McpClientError::CatalogRejected(
            "tool discovery returned an invalid cursor".to_owned(),
        ));
    }
    Ok(())
}

fn map_discovery_error(error: ServiceError) -> McpClientError {
    if matches!(error, ServiceError::Timeout { .. }) {
        McpClientError::DiscoveryTimeout
    } else {
        McpClientError::Discovery(stable_service_error(&error))
    }
}

fn map_call_error(error: ServiceError) -> McpClientError {
    if matches!(error, ServiceError::Timeout { .. }) {
        McpClientError::CallTimeout
    } else {
        McpClientError::Call(stable_service_error(&error))
    }
}

fn map_task_error(error: ServiceError) -> McpClientError {
    match error {
        ServiceError::Timeout { .. } => McpClientError::CallTimeout,
        ServiceError::TransportSend(_) | ServiceError::TransportClosed => {
            McpClientError::TaskTransportLost
        },
        error => McpClientError::Call(stable_service_error(&error)),
    }
}

fn map_controlled_task_error(error: ControlledRequestError) -> McpClientError {
    match error {
        ControlledRequestError::Service(error) => map_task_error(error),
        ControlledRequestError::Cancelled => McpClientError::CallCancelled,
    }
}

fn stable_service_error(error: &ServiceError) -> String {
    match error {
        ServiceError::McpError(error) => {
            format!("remote MCP error code {}", error.code.0)
        },
        ServiceError::TransportSend(_) => "transport send failed".to_owned(),
        ServiceError::TransportClosed => "transport closed".to_owned(),
        ServiceError::UnexpectedResponse => "unexpected response".to_owned(),
        ServiceError::SubscriptionLagged { .. } => "subscription lagged".to_owned(),
        ServiceError::Cancelled { .. } => "request cancelled".to_owned(),
        ServiceError::Timeout { .. } => "request timed out".to_owned(),
        ServiceError::InputRequiredRoundsExceeded { .. } => {
            "input-required round limit exceeded".to_owned()
        },
        _ => "MCP service failure".to_owned(),
    }
}

fn stable_initialize_error(error: &rmcp::service::ClientInitializeError) -> String {
    use rmcp::service::ClientInitializeError;

    match error {
        ClientInitializeError::ExpectedInitResponse(_) => {
            "server returned an invalid initialization response".to_owned()
        },
        ClientInitializeError::ExpectedInitResult(_) => {
            "server returned an invalid initialization result".to_owned()
        },
        ClientInitializeError::ConflictInitResponseId(_, _) => {
            "server returned a conflicting initialization identifier".to_owned()
        },
        ClientInitializeError::ConnectionClosed(_) => {
            "connection closed during initialization".to_owned()
        },
        ClientInitializeError::TransportError { .. } => {
            "transport failed during initialization".to_owned()
        },
        ClientInitializeError::JsonRpcError(error) => {
            format!("remote MCP initialization error code {}", error.code.0)
        },
        ClientInitializeError::NoCompatibleProtocolVersion { .. } => {
            "no compatible MCP protocol version".to_owned()
        },
        ClientInitializeError::NoPreferredProtocolVersion => {
            "client protocol preference was empty".to_owned()
        },
        ClientInitializeError::Cancelled => "initialization cancelled".to_owned(),
        _ => "MCP initialization failed".to_owned(),
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{
        borrow::Cow,
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use rmcp::{
        model::{
            CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock, ContextInclusion,
            CreateMessageRequest, CreateMessageRequestParams, CreateMessageResult,
            CreateTaskResult, DetailedTask, DiscoverResult, ElicitRequest, ElicitRequestParams,
            ElicitResult, ElicitationAction, ElicitationSchema, ErrorCode, ErrorData,
            GetTaskResult, InputRequest, InputRequiredResult, ListRootsRequest, ListRootsResult,
            ListToolsResult, ProtocolVersion, Root, SamplingMessage, ServerCapabilities,
            ServerInfo, ServerNotification, SubscribeRequestParams, SubscriptionFilter, Task,
            TaskPayload, TaskStatus, TaskStatusNotification, Tool, ToolChoice,
            UnsubscribeRequestParams,
        },
        service::{RequestContext, RoleServer, SubscriptionContext, SubscriptionSink},
        ServerHandler, ServiceExt,
    };
    use serde_json::{json, Map, Value};
    use tokio::sync::Notify;
    use tool_runtime_core::credential_profiles::{
        CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    };

    use super::*;
    use crate::{McpContinuationKind, McpPendingCall, McpTaskState};

    fn fixture_tool(name: &str) -> Tool {
        Tool::new(
            name.to_owned(),
            "Fixture tool",
            Arc::new(
                serde_json::from_value(json!({
                    "type": "object",
                    "properties": {"message": {"type": "string"}}
                }))
                .unwrap(),
            ),
        )
    }

    fn task_recovery_binding(resource: &str) -> McpTaskRecoveryBinding {
        let profile = CredentialProfileKey::new(
            CredentialScope::new("person", "space").unwrap(),
            "mcp",
            "primary",
            CredentialProfileBinding::Provider,
        )
        .unwrap();
        McpTaskRecoveryBinding::new(&profile, resource, "fixture-execution/task-one").unwrap()
    }

    #[test]
    fn standard_transports_retain_discover_first_legacy_fallback() {
        let ClientLifecycleMode::Auto {
            preferred_versions,
            legacy_version,
        } = auto_lifecycle()
        else {
            panic!("standard transports must retain Auto lifecycle");
        };
        assert_eq!(
            preferred_versions,
            vec![ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25]
        );
        assert_eq!(legacy_version, Some(ProtocolVersion::V_2025_11_25));
    }

    #[test]
    fn task_transport_loss_has_a_stable_typed_classification() {
        assert!(matches!(
            map_task_error(ServiceError::TransportClosed),
            McpClientError::TaskTransportLost
        ));
        assert!(matches!(
            map_task_error(ServiceError::Timeout {
                timeout: Duration::from_secs(1),
            }),
            McpClientError::CallTimeout
        ));
    }

    fn base_sampling_input() -> InputRequest {
        InputRequest::CreateMessage(CreateMessageRequest::new(CreateMessageRequestParams::new(
            vec![SamplingMessage::user_text("answer")],
            32,
        )))
    }

    fn contextual_sampling_input() -> InputRequest {
        InputRequest::CreateMessage(CreateMessageRequest::new(
            CreateMessageRequestParams::new(vec![SamplingMessage::user_text("answer")], 32)
                .with_include_context(ContextInclusion::ThisServer),
        ))
    }

    fn tool_sampling_input() -> InputRequest {
        InputRequest::CreateMessage(CreateMessageRequest::new(
            CreateMessageRequestParams::new(vec![SamplingMessage::user_text("answer")], 32)
                .with_tools(vec![fixture_tool("sampling-tool")]),
        ))
    }

    fn tool_choice_sampling_input() -> InputRequest {
        InputRequest::CreateMessage(CreateMessageRequest::new(
            CreateMessageRequestParams::new(vec![SamplingMessage::user_text("answer")], 32)
                .with_tool_choice(ToolChoice::required()),
        ))
    }

    fn form_elicitation_input() -> InputRequest {
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Provide a bounded name".to_owned(),
                requested_schema: ElicitationSchema::builder()
                    .required_string_with("name", |schema| schema.length(2, 8))
                    .build()
                    .unwrap(),
            },
        ))
    }

    fn url_elicitation_input() -> InputRequest {
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "Open a remote form".to_owned(),
                url: "https://example.test/form".to_owned(),
                elicitation_id: "canary-url-elicitation".to_owned(),
            },
        ))
    }

    fn roots_input() -> InputRequest {
        InputRequest::ListRoots(ListRootsRequest::default())
    }

    fn input_required_with(
        state: &str,
        requests: impl IntoIterator<Item = (&'static str, InputRequest)>,
    ) -> InputRequiredResult {
        InputRequiredResult::new(
            Some(
                requests
                    .into_iter()
                    .map(|(key, request)| (key.to_owned(), request))
                    .collect(),
            ),
            Some(state.to_owned()),
        )
    }

    fn valid_mrtr_response(kind: crate::McpMrtrInputKind) -> Value {
        match kind {
            crate::McpMrtrInputKind::Sampling => serde_json::to_value(CreateMessageResult::new(
                SamplingMessage::assistant_text("sampled answer"),
                "fixture-model".to_owned(),
            ))
            .unwrap(),
            crate::McpMrtrInputKind::Elicitation => serde_json::to_value(
                ElicitResult::new(ElicitationAction::Accept).with_content(json!({"name": "Ada"})),
            )
            .unwrap(),
            crate::McpMrtrInputKind::Roots => {
                serde_json::to_value(ListRootsResult::new(vec![Root::new("file:///canary/root")]))
                    .unwrap()
            },
        }
    }

    #[derive(Clone)]
    struct FixtureServer;

    #[derive(Default)]
    struct SubscriptionFixtureState {
        catalog_version: AtomicUsize,
        fail_discovery: AtomicBool,
        block_discovery: AtomicBool,
        require_resume: AtomicBool,
        tool_calls: AtomicUsize,
        discovery_started: Notify,
        release_discovery: Notify,
        sink: tokio::sync::Mutex<Option<SubscriptionSink>>,
    }

    #[derive(Clone)]
    struct SubscriptionFixtureServer {
        state: Arc<SubscriptionFixtureState>,
        acknowledge_prompts: bool,
    }

    impl ServerHandler for SubscriptionFixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tool_list_changed()
                    .enable_prompts()
                    .enable_prompts_list_changed()
                    .build(),
            )
            .with_server_info(Implementation::new("subscription-fixture", "1.0.0"))
        }

        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            let mut accepted = requested.supported_by(&self.get_info().capabilities);
            if !self.acknowledge_prompts {
                accepted.prompts_list_changed = None;
            }
            Some(accepted)
        }

        async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
            self.state.sink.lock().await.replace(context.sink().clone());
            context.cancelled().await;
            Ok(())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            if self.state.fail_discovery.swap(false, Ordering::SeqCst) {
                return Err(ErrorData::internal_error("fixture discovery failure", None));
            }
            if self.state.block_discovery.swap(false, Ordering::SeqCst) {
                self.state.discovery_started.notify_one();
                self.state.release_discovery.notified().await;
            }
            let name = if self.state.catalog_version.load(Ordering::SeqCst) == 0 {
                "initial"
            } else {
                "replacement"
            };
            Ok(ListToolsResult {
                tools: vec![fixture_tool(name)],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if request.name != "initial" && request.name != "replacement" {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "unknown fixture tool",
                    None,
                ));
            }
            if self.state.require_resume.load(Ordering::SeqCst)
                && self.state.tool_calls.fetch_add(1, Ordering::SeqCst) == 0
            {
                return Ok(InputRequiredResult::from_request_state("private-resume-state").into());
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(request.name)]).into())
        }
    }

    #[derive(Default)]
    struct LegacySubscriptionFixtureState {
        subscribed: Mutex<Vec<String>>,
        unsubscribed: Mutex<Vec<String>>,
    }

    #[derive(Clone)]
    struct LegacySubscriptionFixtureServer {
        state: Arc<LegacySubscriptionFixtureState>,
    }

    #[derive(Clone)]
    struct EndingSubscriptionFixtureServer;

    impl ServerHandler for EndingSubscriptionFixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tool_list_changed()
                    .build(),
            )
        }

        fn accepted_subscription_filter(
            &self,
            requested: &SubscriptionFilter,
        ) -> Option<SubscriptionFilter> {
            Some(requested.supported_by(&self.get_info().capabilities))
        }

        async fn listen(&self, _context: SubscriptionContext) -> Result<(), ErrorData> {
            Ok(())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("ending")],
                ..Default::default()
            })
        }
    }

    #[allow(deprecated)]
    impl ServerHandler for LegacySubscriptionFixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(
                ServerCapabilities::builder()
                    .enable_resources()
                    .enable_resources_subscribe()
                    .build(),
            )
            .with_server_info(Implementation::new("legacy-subscription-fixture", "1.0.0"))
        }

        async fn subscribe(
            &self,
            request: SubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            self.state.subscribed.lock().unwrap().push(request.uri);
            Ok(())
        }

        async fn unsubscribe(
            &self,
            request: UnsubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            self.state.unsubscribed.lock().unwrap().push(request.uri);
            Ok(())
        }
    }

    #[derive(Default)]
    struct TaskFixtureState {
        polls: AtomicUsize,
        updates: Mutex<Vec<rmcp::model::InputResponses>>,
        cancelled: AtomicBool,
        client_advertised_tasks: AtomicBool,
        fail_next_poll: AtomicBool,
        wrong_identity: AtomicBool,
    }

    #[derive(Clone)]
    struct TaskFixtureServer {
        state: Arc<TaskFixtureState>,
        poll_interval_ms: u64,
        notify_after_create: bool,
    }

    impl TaskFixtureServer {
        fn task(&self, status: TaskStatus) -> Task {
            Task::new(
                "private-task-id",
                status,
                "2026-08-07T00:00:00Z",
                "2026-08-07T00:00:01Z",
            )
            .with_ttl_ms(60_000)
            .with_poll_interval_ms(self.poll_interval_ms)
        }
    }

    impl ServerHandler for TaskFixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_tasks()
                    .build(),
            )
            .with_server_info(Implementation::new("task-fixture", "1.0.0"))
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("task-tool")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.state.client_advertised_tasks.store(
                context
                    .client_capabilities()
                    .is_some_and(|capabilities| capabilities.supports_tasks()),
                Ordering::SeqCst,
            );
            if self.notify_after_create {
                let peer = context.peer.clone();
                let notification = TaskStatusNotification::new(TaskStatusNotificationParams::new(
                    DetailedTask::new(self.task(TaskStatus::Working), TaskPayload::Working),
                ));
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _ = peer
                        .send_notification(ServerNotification::TaskStatusNotification(notification))
                        .await;
                });
            }
            Ok(CreateTaskResult::new(self.task(TaskStatus::Working)).into())
        }

        async fn get_task(
            &self,
            request: GetTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<GetTaskResult, ErrorData> {
            if request.task_id != "private-task-id" {
                return Err(ErrorData::invalid_params("unknown task", None));
            }
            if self.state.fail_next_poll.swap(false, Ordering::SeqCst) {
                return Err(ErrorData::internal_error("fixture transport loss", None));
            }
            if self.state.cancelled.load(Ordering::SeqCst) {
                return Ok(GetTaskResult::new(DetailedTask::new(
                    self.task(TaskStatus::Cancelled),
                    TaskPayload::Cancelled,
                )));
            }
            let poll = self.state.polls.fetch_add(1, Ordering::SeqCst);
            if self.state.updates.lock().unwrap().is_empty() {
                let requests = BTreeMap::from([(
                    "private-root-key".to_owned(),
                    InputRequest::ListRoots(ListRootsRequest::default()),
                )]);
                let mut task = self.task(TaskStatus::InputRequired);
                if self.state.wrong_identity.load(Ordering::SeqCst) {
                    task.task_id = "different-private-task-id".to_owned();
                }
                return Ok(GetTaskResult::new(DetailedTask::new(
                    task,
                    TaskPayload::InputRequired {
                        input_requests: requests,
                    },
                )));
            }
            if poll <= 1 {
                return Ok(GetTaskResult::new(DetailedTask::new(
                    self.task(TaskStatus::Working),
                    TaskPayload::Working,
                )));
            }
            let result = serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                "task-finished",
            )]))
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
            Ok(GetTaskResult::new(DetailedTask::new(
                self.task(TaskStatus::Completed),
                TaskPayload::Completed { result },
            )))
        }

        async fn update_task(
            &self,
            request: UpdateTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            if request.task_id != "private-task-id"
                || !request.input_responses.contains_key("private-root-key")
            {
                return Err(ErrorData::invalid_params("invalid task update", None));
            }
            self.state
                .updates
                .lock()
                .unwrap()
                .push(request.input_responses);
            Ok(())
        }

        async fn cancel_task(
            &self,
            request: CancelTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            if request.task_id != "private-task-id" {
                return Err(ErrorData::invalid_params("unknown task", None));
            }
            self.state.cancelled.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    impl ServerHandler for FixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new("fixture-server", "1.0.0"))
        }

        async fn list_tools(
            &self,
            request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let mut response = ListToolsResult::default();
            if request.and_then(|params| params.cursor).as_deref() == Some("page-2") {
                response.tools = vec![fixture_tool("reverse")];
            } else {
                response.tools = vec![fixture_tool("echo")];
                response.next_cursor = Some("page-2".to_owned());
            }
            Ok(response)
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if request.name != "echo" && request.name != "reverse" {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "unknown fixture tool",
                    None,
                ));
            }
            let message = request
                .arguments
                .and_then(|arguments| arguments.get("message").cloned())
                .unwrap_or(Value::Null);
            let mut text = message.as_str().unwrap_or_default().to_owned();
            if request.name == "reverse" {
                text = text.chars().rev().collect();
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
        }
    }

    #[derive(Clone)]
    struct LegacyFixtureServer;

    #[derive(Clone)]
    struct InputRequiredServer {
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for InputRequiredServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new("input-required-fixture", "1.0.0"))
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(InputRequiredResult::from_request_state("canary-provider-request-state").into())
        }
    }

    #[derive(Clone)]
    struct MultiRoundInputServer {
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<CallToolRequestParams>>>,
    }

    #[derive(Clone)]
    struct RootsInputServer {
        calls: Arc<AtomicUsize>,
        retry: Arc<Mutex<Option<CallToolRequestParams>>>,
    }

    impl ServerHandler for RootsInputServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let input_requests = BTreeMap::from([(
                    "canary-private-root-key".to_owned(),
                    InputRequest::ListRoots(ListRootsRequest::default()),
                )]);
                return Ok(InputRequiredResult::new(
                    Some(input_requests),
                    Some("canary-root-state".to_owned()),
                )
                .into());
            }
            *self.retry.lock().unwrap() = Some(request);
            Ok(CallToolResult::success(vec![ContentBlock::text("roots-finished")]).into())
        }
    }

    impl ServerHandler for MultiRoundInputServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request);
            match round {
                0 => Ok(InputRequiredResult::from_request_state("canary-round-one").into()),
                1 => Ok(InputRequiredResult::from_request_state("canary-round-two").into()),
                _ => Ok(CallToolResult::success(vec![ContentBlock::text("finished")]).into()),
            }
        }
    }

    #[derive(Clone)]
    struct FailingResumeServer {
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for FailingResumeServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(InputRequiredResult::from_request_state("canary-failure-state").into());
            }
            Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                "canary-retry-secret-message",
                Some(json!({"secret": "canary-retry-secret-data"})),
            ))
        }
    }

    #[derive(Clone)]
    struct BlockingResumeServer {
        calls: Arc<AtomicUsize>,
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    }

    impl ServerHandler for BlockingResumeServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(InputRequiredResult::from_request_state("canary-blocking-state").into());
            }
            self.started.notify_waiters();
            context.ct.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, "cancelled", None))
        }
    }

    #[derive(Clone)]
    struct InteractiveInputServer {
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for InteractiveInputServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("needs-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(InputRequiredResult::new(
                Some(BTreeMap::from([(
                    "canary-private-root-key".to_owned(),
                    InputRequest::ListRoots(ListRootsRequest::default()),
                )])),
                Some(format!("canary-interactive-round-{round}")),
            )
            .into())
        }
    }

    #[derive(Clone)]
    struct CapabilityProbeServer {
        observed: Arc<Mutex<Vec<ClientCapabilities>>>,
    }

    impl ServerHandler for CapabilityProbeServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::new(AtomicUsize::new(0)),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("capability-probe")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.observed
                .lock()
                .unwrap()
                .push(context.client_capabilities().unwrap_or_default());
            Ok(CallToolResult::success(vec![ContentBlock::text("observed")]).into())
        }
    }

    #[derive(Clone)]
    struct FixedInputServer {
        result: InputRequiredResult,
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for FixedInputServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("fixed-input")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.result.clone().into())
        }
    }

    #[derive(Clone)]
    struct SupportedThenUnsupportedServer {
        calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for SupportedThenUnsupportedServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("supported-then-unsupported")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst);
            let result = if round == 0 {
                input_required_with("roots-state", [("private-roots", roots_input())])
            } else {
                input_required_with(
                    "unsupported-url-state",
                    [("private-url", url_elicitation_input())],
                )
            };
            Ok(result.into())
        }
    }

    #[derive(Clone)]
    struct QualifiedMultiRoundServer {
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<CallToolRequestParams>>>,
    }

    impl ServerHandler for QualifiedMultiRoundServer {
        fn get_info(&self) -> ServerInfo {
            InputRequiredServer {
                calls: Arc::clone(&self.calls),
            }
            .get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("qualified-multi-round")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request);
            match round {
                0 => Ok(InputRequiredResult::from_request_state("state-only").into()),
                1 => Ok(input_required_with(
                    "sampling-state",
                    [("private-sampling", base_sampling_input())],
                )
                .into()),
                2 => Ok(input_required_with(
                    "form-state",
                    [("private-form", form_elicitation_input())],
                )
                .into()),
                3 => Ok(
                    input_required_with("roots-state", [("private-roots", roots_input())]).into(),
                ),
                _ => Ok(CallToolResult::success(vec![ContentBlock::text("qualified")]).into()),
            }
        }
    }

    #[derive(Clone)]
    struct ConcurrentMrtrServer {
        requests: Arc<Mutex<Vec<CallToolRequestParams>>>,
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
    }

    impl ServerHandler for ConcurrentMrtrServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("concurrent-mrtr")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let lane = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("lane"))
                .and_then(Value::as_str)
                .unwrap_or("missing")
                .to_owned();
            let is_initial = request.request_state.is_none();
            self.requests.lock().unwrap().push(request);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            if is_initial {
                let key = format!("private-root-{lane}");
                return Ok(InputRequiredResult::new(
                    Some(BTreeMap::from([(key, roots_input())])),
                    Some(format!("state-{lane}")),
                )
                .into());
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(lane)]).into())
        }
    }

    impl ServerHandler for LegacyFixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_protocol_version(ProtocolVersion::V_2025_11_25)
                .with_server_info(Implementation::new("legacy-fixture", "1.0.0"))
        }

        fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
            Cow::Owned(vec![ProtocolVersion::V_2025_11_25])
        }

        async fn discover(
            &self,
            _context: RequestContext<RoleServer>,
        ) -> Result<DiscoverResult, ErrorData> {
            Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "server/discover is unavailable",
                None,
            ))
        }
    }

    #[derive(Clone)]
    struct RepeatingCursorServer;

    impl ServerHandler for RepeatingCursorServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                next_cursor: Some("repeat".to_owned()),
                ..Default::default()
            })
        }
    }

    #[derive(Clone)]
    struct EndlessCursorServer;

    impl ServerHandler for EndlessCursorServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let current = request
                .and_then(|params| params.cursor)
                .and_then(|cursor| cursor.parse::<usize>().ok())
                .unwrap_or_default();
            Ok(ListToolsResult {
                next_cursor: Some(current.saturating_add(1).to_string()),
                ..Default::default()
            })
        }
    }

    #[derive(Clone)]
    struct FailingToolServer;

    impl ServerHandler for FailingToolServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("fail")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                "canary-secret-message",
                Some(json!({"credential": "canary-secret-data"})),
            ))
        }
    }

    #[derive(Clone)]
    struct FailingRediscoveryServer {
        list_calls: Arc<AtomicUsize>,
    }

    impl ServerHandler for FailingRediscoveryServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            if self.list_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(ListToolsResult {
                    tools: vec![fixture_tool("stable")],
                    ..Default::default()
                });
            }
            Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                "rediscovery-secret-canary",
                None,
            ))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if request.name != "stable" {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "unknown fixture tool",
                    None,
                ));
            }
            Ok(CallToolResult::success(vec![ContentBlock::text("stable")]).into())
        }
    }

    #[derive(Clone)]
    struct BlockingRediscoveryServer {
        list_calls: Arc<AtomicUsize>,
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    }

    impl ServerHandler for BlockingRediscoveryServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            if self.list_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(ListToolsResult {
                    tools: vec![fixture_tool("stable")],
                    ..Default::default()
                });
            }
            self.started.notify_waiters();
            context.ct.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, "cancelled", None))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            if request.name != "stable" {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "unknown fixture tool",
                    None,
                ));
            }
            Ok(CallToolResult::success(vec![ContentBlock::text("stable")]).into())
        }
    }

    #[derive(Clone)]
    struct BlockingToolServer {
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    }

    impl ServerHandler for BlockingToolServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("block")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.started.notify_waiters();
            context.ct.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, "cancelled", None))
        }
    }

    #[derive(Clone)]
    struct ConcurrentToolServer {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
    }

    impl ServerHandler for ConcurrentToolServer {
        fn get_info(&self) -> ServerInfo {
            FixtureServer.get_info()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult {
                tools: vec![fixture_tool("concurrent")],
                ..Default::default()
            })
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
        }
    }

    async fn connect_fixture<S>(
        server: S,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        connect_fixture_with(server, McpClientLimits::default(), Duration::from_secs(2)).await
    }

    async fn connect_fixture_with<S>(
        server: S,
        limits: McpClientLimits,
        request_timeout: Duration,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        connect_fixture_with_presentation(
            server,
            limits,
            request_timeout,
            McpMrtrPresentationCapabilities::default(),
        )
        .await
    }

    async fn connect_fixture_with_presentation<S>(
        server: S,
        limits: McpClientLimits,
        request_timeout: Duration,
        mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        connect_fixture_with_capabilities(
            server,
            limits,
            request_timeout,
            mrtr_presentation_capabilities,
            McpTaskLifecycleCapabilities::default(),
            McpSubscriptionCapabilities::default(),
        )
        .await
    }

    async fn connect_fixture_with_capabilities<S>(
        server: S,
        limits: McpClientLimits,
        request_timeout: Duration,
        mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
        task_lifecycle_capabilities: McpTaskLifecycleCapabilities,
        subscription_capabilities: McpSubscriptionCapabilities,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let service = server
                .serve(server_io)
                .await
                .map_err(|error| error.to_string())?;
            service.waiting().await.map_err(|error| error.to_string())?;
            Ok(())
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = crate::stdio_transport::BoundedAsyncRwTransport::new(
            client_read,
            client_write,
            limits.max_transport_message_bytes,
        );
        let task_hints = Arc::new(McpTaskNotificationHints::new(
            limits.max_pending_continuations,
        ));
        let invalidations = Arc::new(McpInvalidationOwner::new());
        let running = connect_service(
            transport,
            "fixture",
            Duration::from_secs(2),
            auto_lifecycle(),
            mrtr_presentation_capabilities,
            task_lifecycle_capabilities,
            Arc::clone(&task_hints),
            Arc::clone(&invalidations),
        )
        .await
        .unwrap();
        let client = McpClient::from_running_unbudgeted(
            running,
            "fixture",
            McpRunningClientConfig {
                limits,
                request_timeout,
                shutdown_timeout: Duration::from_secs(2),
                continuation_timeout: Duration::from_secs(60),
                mrtr_presentation_capabilities,
                task_lifecycle_capabilities,
                task_poll_floor: Duration::from_millis(1),
                task_hints,
                invalidations,
                subscription_capabilities,
                subscription_channel_capacity: 64,
            },
        )
        .await
        .unwrap();
        (client, server_task)
    }

    #[tokio::test]
    async fn failed_process_budget_admission_closes_the_connected_service() {
        let mut limits = McpClientLimits::default();
        limits.max_continuation_bytes =
            crate::continuation::MAX_PROCESS_CONTINUATION_BYTES.saturating_add(1);
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let service = FixtureServer
                .serve(server_io)
                .await
                .map_err(|error| error.to_string())?;
            service.waiting().await.map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = crate::stdio_transport::BoundedAsyncRwTransport::new(
            client_read,
            client_write,
            limits.max_transport_message_bytes,
        );
        let task_hints = Arc::new(McpTaskNotificationHints::new(
            limits.max_pending_continuations,
        ));
        let invalidations = Arc::new(McpInvalidationOwner::new());
        let running = connect_service(
            transport,
            "fixture",
            Duration::from_secs(2),
            auto_lifecycle(),
            McpMrtrPresentationCapabilities::default(),
            McpTaskLifecycleCapabilities::default(),
            Arc::clone(&task_hints),
            Arc::clone(&invalidations),
        )
        .await
        .expect("connect fixture service");

        let result = McpClient::from_running_with_budget(
            running,
            "fixture",
            McpRunningClientConfig {
                limits,
                request_timeout: Duration::from_secs(2),
                shutdown_timeout: Duration::from_secs(2),
                continuation_timeout: Duration::from_secs(60),
                mrtr_presentation_capabilities: McpMrtrPresentationCapabilities::default(),
                task_lifecycle_capabilities: McpTaskLifecycleCapabilities::default(),
                task_poll_floor: Duration::from_millis(1),
                task_hints,
                invalidations,
                subscription_capabilities: McpSubscriptionCapabilities::default(),
                subscription_channel_capacity: 64,
            },
            true,
        )
        .await;
        assert!(matches!(
            result,
            Err(McpClientError::ContinuationCapacityExceeded)
        ));

        let server_result = tokio::time::timeout(Duration::from_secs(2), server_task)
            .await
            .expect("rejected client must close the connected service")
            .expect("fixture server task must not panic");
        assert!(server_result.is_ok(), "fixture service close failed");
    }

    async fn connect_subscription_fixture<S>(
        server: S,
        subscription_capabilities: McpSubscriptionCapabilities,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        connect_fixture_with_capabilities(
            server,
            McpClientLimits::default(),
            Duration::from_secs(2),
            McpMrtrPresentationCapabilities::default(),
            McpTaskLifecycleCapabilities::default(),
            subscription_capabilities,
        )
        .await
    }

    async fn connect_legacy_subscription_fixture<S>(
        server: S,
        subscription_capabilities: McpSubscriptionCapabilities,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>)
    where
        S: ServerHandler + Send + Sync + 'static,
    {
        let limits = McpClientLimits::default();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let service = server
                .serve(server_io)
                .await
                .map_err(|error| error.to_string())?;
            service.waiting().await.map_err(|error| error.to_string())?;
            Ok(())
        });
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = crate::stdio_transport::BoundedAsyncRwTransport::new(
            client_read,
            client_write,
            limits.max_transport_message_bytes,
        );
        let task_hints = Arc::new(McpTaskNotificationHints::new(
            limits.max_pending_continuations,
        ));
        let invalidations = Arc::new(McpInvalidationOwner::new());
        let handler = McpClientHandler {
            info: ClientInfo::new(
                ClientCapabilities::default(),
                Implementation::new("legacy-client-fixture", "1.0.0"),
            )
            .with_protocol_version(ProtocolVersion::V_2025_11_25),
            task_hints: Arc::clone(&task_hints),
            invalidations: Arc::clone(&invalidations),
        };
        let running = handler.serve(transport).await.unwrap();
        let client = McpClient::from_running_unbudgeted(
            running,
            "legacy-fixture",
            McpRunningClientConfig {
                limits,
                request_timeout: Duration::from_secs(2),
                shutdown_timeout: Duration::from_secs(2),
                continuation_timeout: Duration::from_secs(60),
                mrtr_presentation_capabilities: McpMrtrPresentationCapabilities::default(),
                task_lifecycle_capabilities: McpTaskLifecycleCapabilities::default(),
                task_poll_floor: Duration::from_millis(1),
                task_hints,
                invalidations,
                subscription_capabilities,
                subscription_channel_capacity: 64,
            },
        )
        .await
        .unwrap();
        (client, server_task)
    }

    async fn notify_tool_change(state: &SubscriptionFixtureState) {
        let sink = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(sink) = state.sink.lock().await.clone() {
                    break sink;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("subscription sink was not installed");
        sink.notify_tool_list_changed()
            .await
            .expect("tool invalidation notification");
    }

    #[tokio::test]
    async fn notification_subscriptions_are_default_off_and_exactly_acknowledged() {
        let state = Arc::new(SubscriptionFixtureState::default());
        let server = SubscriptionFixtureServer {
            state: Arc::clone(&state),
            acknowledge_prompts: true,
        };
        let (client, server_task) = connect_fixture(server).await;
        assert!(client.subscription_capabilities().is_empty());
        assert!(client.negotiated_subscription_capabilities().is_empty());
        let error = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap_err();
        assert!(matches!(error, McpClientError::SubscriptionUnsupported));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();

        let state = Arc::new(SubscriptionFixtureState::default());
        let caps = McpSubscriptionCapabilities::new()
            .enable_tools_list_changed()
            .enable_prompts_list_changed();
        let (client, server_task) = connect_subscription_fixture(
            SubscriptionFixtureServer {
                state,
                acknowledge_prompts: false,
            },
            caps,
        )
        .await;
        let error = client
            .open_notification_subscription(
                McpSubscriptionRequest::new()
                    .with_tools_list_changed()
                    .with_prompts_list_changed(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            McpClientError::SubscriptionProtocolViolation
        ));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn legacy_resource_subscription_uses_official_subscribe_and_unsubscribe() {
        let state = Arc::new(LegacySubscriptionFixtureState::default());
        let (client, server_task) = connect_legacy_subscription_fixture(
            LegacySubscriptionFixtureServer {
                state: Arc::clone(&state),
            },
            McpSubscriptionCapabilities::new().enable_resource_updates(),
        )
        .await;
        assert!(!client.current_subscriptions);
        let subscription = client
            .open_notification_subscription(
                McpSubscriptionRequest::new().with_resource_uri("file:///private/notes"),
            )
            .await
            .unwrap();
        assert!(subscription.state().unwrap().resource_updates());
        assert_eq!(
            state.subscribed.lock().unwrap().as_slice(),
            ["file:///private/notes"]
        );
        subscription.cancel().await.unwrap();
        assert_eq!(
            state.unsubscribed.lock().unwrap().as_slice(),
            ["file:///private/notes"]
        );
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn subscription_end_marks_catalog_dirty_and_releases_the_active_lease() {
        let (mut client, server_task) = connect_subscription_fixture(
            EndingSubscriptionFixtureServer,
            McpSubscriptionCapabilities::new().enable_tools_list_changed(),
        )
        .await;
        let mut subscription = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap();
        client.discover_tools().await.unwrap();
        assert!(!client.invalidation_state().unwrap().tools_changed());
        assert!(matches!(
            subscription.next().await,
            Err(McpClientError::SubscriptionEnded)
        ));
        let state = client.invalidation_state().unwrap();
        assert!(!state.subscription_active());
        assert!(state.tools_changed());
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invalidation_refresh_replaces_authority_and_rejects_old_ids() {
        let state = Arc::new(SubscriptionFixtureState::default());
        let caps = McpSubscriptionCapabilities::new().enable_tools_list_changed();
        let (mut client, server_task) = connect_subscription_fixture(
            SubscriptionFixtureServer {
                state: Arc::clone(&state),
                acknowledge_prompts: true,
            },
            caps,
        )
        .await;
        let mut subscription = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap();
        assert!(subscription.state().unwrap().tools_changed());
        let initial = client.discover_tools().await.unwrap();
        let old_id = initial[0].id().clone();
        assert!(!client.invalidation_state().unwrap().tools_changed());
        assert!(client
            .refresh_tools_if_invalidated()
            .await
            .unwrap()
            .is_none());

        let duplicate = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap_err();
        assert!(matches!(
            duplicate,
            McpClientError::SubscriptionAlreadyActive
        ));

        state.catalog_version.store(1, Ordering::SeqCst);
        notify_tool_change(&state).await;
        subscription.next().await.unwrap();
        let replacement = client
            .refresh_tools_if_invalidated()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replacement[0].id().remote_name(), "replacement");
        assert!(!client.invalidation_state().unwrap().tools_changed());
        let error = client.call_tool(&old_id, Map::new()).await.unwrap_err();
        assert!(matches!(error, McpClientError::ToolNotDiscovered));
        client
            .call_tool(replacement[0].id(), Map::new())
            .await
            .unwrap();

        subscription.cancel().await.unwrap();
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn successful_refresh_preserves_an_existing_continuation() {
        let state = Arc::new(SubscriptionFixtureState::default());
        state.require_resume.store(true, Ordering::SeqCst);
        let caps = McpSubscriptionCapabilities::new().enable_tools_list_changed();
        let (mut client, server_task) = connect_subscription_fixture(
            SubscriptionFixtureServer {
                state: Arc::clone(&state),
                acknowledge_prompts: true,
            },
            caps,
        )
        .await;
        let mut subscription = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap();
        let initial = client.discover_tools().await.unwrap();
        let pending = match client.call_tool(initial[0].id(), Map::new()).await.unwrap() {
            McpToolCallOutcome::InputRequired(pending) => pending,
            other => panic!("expected retained continuation, got {other:?}"),
        };

        state.catalog_version.store(1, Ordering::SeqCst);
        notify_tool_change(&state).await;
        subscription.next().await.unwrap();
        client
            .refresh_tools_if_invalidated()
            .await
            .unwrap()
            .unwrap();
        let prepared = client.prepare_mrtr_responses(pending, Vec::new()).unwrap();
        let claim = client.claim_mrtr_responses(prepared).unwrap();
        assert!(matches!(
            client.resume_mrtr(claim).await.unwrap(),
            McpToolCallOutcome::Complete(_)
        ));

        subscription.cancel().await.unwrap();
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_and_racing_refreshes_preserve_authority_and_dirty_revisions() {
        let state = Arc::new(SubscriptionFixtureState::default());
        let caps = McpSubscriptionCapabilities::new().enable_tools_list_changed();
        let (mut client, server_task) = connect_subscription_fixture(
            SubscriptionFixtureServer {
                state: Arc::clone(&state),
                acknowledge_prompts: true,
            },
            caps,
        )
        .await;
        let mut subscription = client
            .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
            .await
            .unwrap();
        let initial = client.discover_tools().await.unwrap();
        let old_id = initial[0].id().clone();

        notify_tool_change(&state).await;
        subscription.next().await.unwrap();
        state.fail_discovery.store(true, Ordering::SeqCst);
        assert!(client.refresh_tools_if_invalidated().await.is_err());
        assert!(client.invalidation_state().unwrap().tools_changed());
        client.call_tool(&old_id, Map::new()).await.unwrap();

        state.catalog_version.store(1, Ordering::SeqCst);
        state.block_discovery.store(true, Ordering::SeqCst);
        let mut refresh = Box::pin(client.refresh_tools_if_invalidated());
        tokio::select! {
            _ = state.discovery_started.notified() => {},
            result = &mut refresh => panic!("refresh completed before fixture release: {result:?}"),
        }
        notify_tool_change(&state).await;
        subscription.next().await.unwrap();
        state.release_discovery.notify_one();
        let replacement = refresh.await.unwrap().unwrap();
        assert_eq!(replacement[0].id().remote_name(), "replacement");
        assert!(client.invalidation_state().unwrap().tools_changed());
        assert!(client
            .refresh_tools_if_invalidated()
            .await
            .unwrap()
            .is_some());
        assert!(!client.invalidation_state().unwrap().tools_changed());

        subscription.cancel().await.unwrap();
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    async fn assert_input_shape_rejected(
        result: InputRequiredResult,
        presentation: McpMrtrPresentationCapabilities,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture_with_presentation(
            FixedInputServer {
                result,
                calls: Arc::clone(&calls),
            },
            McpClientLimits::default(),
            Duration::from_secs(2),
            presentation,
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let error = client
            .call_tool(tools[0].id(), Map::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            McpClientError::ContinuationCapabilityUnsupported
        ));
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    async fn wait_for_flag(flag: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !flag.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture did not observe SDK cancellation");
    }

    async fn connect_task_fixture(
        server: TaskFixtureServer,
    ) -> (McpClient, tokio::task::JoinHandle<Result<(), String>>) {
        connect_fixture_with_capabilities(
            server,
            McpClientLimits::default(),
            Duration::from_secs(2),
            McpMrtrPresentationCapabilities::new().enable_roots(),
            McpTaskLifecycleCapabilities::new().enable(),
            McpSubscriptionCapabilities::default(),
        )
        .await
    }

    async fn create_fixture_task(client: &mut McpClient) -> McpPendingCall {
        let tools = client.discover_tools().await.unwrap();
        match client.call_tool(tools[0].id(), Map::new()).await.unwrap() {
            McpToolCallOutcome::Task(pending) => pending,
            other => panic!("expected retained task, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tasks_are_explicitly_and_bilaterally_negotiated() {
        let state = Arc::new(TaskFixtureState::default());
        let server = TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        };
        let (mut client, server_task) = connect_task_fixture(server).await;
        assert!(client.task_lifecycle_capabilities().enabled());
        assert!(client.task_lifecycle_negotiated());
        let pending = create_fixture_task(&mut client).await;
        assert!(state.client_advertised_tasks.load(Ordering::SeqCst));
        assert_eq!(pending.kind(), McpContinuationKind::RemoteTask);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();

        let state = Arc::new(TaskFixtureState::default());
        let (client, server_task) = connect_fixture(TaskFixtureServer {
            state,
            poll_interval_ms: 1,
            notify_after_create: false,
        })
        .await;
        assert!(!client.task_lifecycle_capabilities().enabled());
        assert!(!client.task_lifecycle_negotiated());
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn official_task_lifecycle_updates_input_and_retains_until_terminal_get() {
        let state = Arc::new(TaskFixtureState::default());
        let (mut client, server_task) = connect_task_fixture(TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        })
        .await;
        let mut pending = create_fixture_task(&mut client).await;
        assert_eq!(
            client.pending_task_progress(pending).unwrap().state(),
            McpTaskState::Working
        );

        tokio::time::sleep(Duration::from_millis(2)).await;
        pending = match client.poll_task(pending).await.unwrap() {
            McpTaskPollOutcome::Pending(progress) => {
                assert_eq!(progress.state(), McpTaskState::InputRequired);
                progress.pending()
            },
            other => panic!("expected task input, got {other:?}"),
        };
        let slots = client.pending_task_input_slots(pending).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].kind(), crate::McpMrtrInputKind::Roots);
        let response = serde_json::to_value(ListRootsResult::new(vec![Root::new(
            "file:///governed/root",
        )]))
        .unwrap();
        let prepared = client
            .prepare_task_responses(pending, vec![McpMrtrResponse::new(slots[0].id(), response)])
            .unwrap();
        pending = client.update_task(prepared).await.unwrap();
        assert_eq!(
            client.pending_task_progress(pending).unwrap().state(),
            McpTaskState::Working
        );
        assert_eq!(state.updates.lock().unwrap().len(), 1);

        tokio::time::sleep(Duration::from_millis(2)).await;
        pending = match client.poll_task(pending).await.unwrap() {
            McpTaskPollOutcome::Pending(progress) => progress.pending(),
            other => panic!("expected working task, got {other:?}"),
        };
        tokio::time::sleep(Duration::from_millis(2)).await;
        match client.poll_task(pending).await.unwrap() {
            McpTaskPollOutcome::Complete(result) => {
                assert_eq!(result.content[0]["text"], "task-finished");
            },
            other => panic!("expected completed task, got {other:?}"),
        }
        assert!(!client.pending_call_is_active(pending).unwrap());
        assert_eq!(client.pending_call_count().unwrap(), 0);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn task_cancellation_ack_is_not_terminal_authority() {
        let state = Arc::new(TaskFixtureState::default());
        let (mut client, server_task) = connect_task_fixture(TaskFixtureServer {
            state,
            poll_interval_ms: 1,
            notify_after_create: false,
        })
        .await;
        let pending = create_fixture_task(&mut client).await;
        let pending = client.cancel_task(pending).await.unwrap();
        assert_eq!(
            client.pending_task_progress(pending).unwrap().state(),
            McpTaskState::CancellationRequested
        );
        assert!(matches!(
            client.cancel_task(pending).await,
            Err(McpClientError::TaskCancellationAlreadyRequested)
        ));
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert!(matches!(
            client.poll_task(pending).await.unwrap(),
            McpTaskPollOutcome::Cancelled
        ));
        assert_eq!(client.pending_call_count().unwrap(), 0);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn task_notification_is_only_a_coalesced_early_poll_hint() {
        let state = Arc::new(TaskFixtureState::default());
        let (mut client, server_task) = connect_task_fixture(TaskFixtureServer {
            state,
            poll_interval_ms: 60_000,
            notify_after_create: true,
        })
        .await;
        let pending = create_fixture_task(&mut client).await;
        assert_eq!(
            client.pending_task_progress(pending).unwrap().state(),
            McpTaskState::Working
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if client
                    .pending_task_progress(pending)
                    .unwrap()
                    .poll_after()
                    .is_zero()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        match client.poll_task(pending).await.unwrap() {
            McpTaskPollOutcome::Pending(progress) => {
                assert_eq!(progress.state(), McpTaskState::InputRequired);
            },
            other => panic!("notification became authority: {other:?}"),
        }
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn concurrent_task_polls_have_one_exact_revision_winner() {
        let state = Arc::new(TaskFixtureState::default());
        let (mut client, server_task) = connect_task_fixture(TaskFixtureServer {
            state,
            poll_interval_ms: 1,
            notify_after_create: false,
        })
        .await;
        let pending = create_fixture_task(&mut client).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        let (first, second) = tokio::join!(client.poll_task(pending), client.poll_task(pending));
        let successes = usize::from(first.is_ok()) + usize::from(second.is_ok());
        assert_eq!(successes, 1);
        let failure = if first.is_err() { first } else { second };
        assert!(matches!(
            failure,
            Err(McpClientError::ContinuationNotActive)
        ));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn restart_classification_keeps_mrtr_session_bound() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture(InputRequiredServer {
            calls: Arc::clone(&calls),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let pending = match client.call_tool(tools[0].id(), Map::new()).await.unwrap() {
            McpToolCallOutcome::InputRequired(pending) => pending,
            other => panic!("expected MRTR continuation, got {other:?}"),
        };
        assert_eq!(
            client.continuation_recovery_disposition(pending).unwrap(),
            crate::McpContinuationRecoveryDisposition::SessionBound
        );
        assert!(matches!(
            client.checkpoint_task(task_recovery_binding("mcp://fixture"), pending),
            Err(McpClientError::ContinuationCapabilityUnsupported)
                | Err(McpClientError::ContinuationRecoverySessionBound)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn restart_recovery_rebinds_exactly_and_revalidates_before_retention() {
        let state = Arc::new(TaskFixtureState::default());
        let server = || TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        };
        let binding = task_recovery_binding("mcp://task-fixture/primary");
        let (mut first, first_server) = connect_task_fixture(server()).await;
        let old_pending = create_fixture_task(&mut first).await;
        assert_eq!(
            first
                .continuation_recovery_disposition(old_pending)
                .unwrap(),
            crate::McpContinuationRecoveryDisposition::RecoverableRemoteTask
        );
        let checkpoint = first.checkpoint_task(binding, old_pending).unwrap();
        let debug = format!("{checkpoint:?}");
        assert!(!debug.contains("private-task-id"));
        assert!(!debug.contains("task-tool"));
        first.close().await.unwrap();
        first_server.await.unwrap().unwrap();

        let (mut second, second_server) = connect_task_fixture(server()).await;
        let tools = second.discover_tools().await.unwrap();
        assert!(matches!(
            second.prepare_task_recovery(
                task_recovery_binding("mcp://task-fixture/other"),
                &checkpoint,
                tools[0].id(),
            ),
            Err(McpClientError::TaskRecoveryRecordRejected)
        ));
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);

        let mut wrong_server_record = checkpoint.decode(binding).unwrap();
        wrong_server_record.server_version = Some("different".to_owned());
        let wrong_server = McpTaskRecoveryCheckpoint::encode(&wrong_server_record).unwrap();
        assert!(matches!(
            second.prepare_task_recovery(binding, &wrong_server, tools[0].id()),
            Err(McpClientError::TaskRecoveryRecordRejected)
        ));
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);

        let mut exhausted_record = checkpoint.decode(binding).unwrap();
        exhausted_record.operation_count = second.limits.max_task_operations as u64;
        let exhausted = McpTaskRecoveryCheckpoint::encode(&exhausted_record).unwrap();
        assert!(matches!(
            second.prepare_task_recovery(binding, &exhausted, tools[0].id()),
            Err(McpClientError::TaskOperationLimitExceeded)
        ));
        let mut expired_record = checkpoint.decode(binding).unwrap();
        expired_record.expires_at_epoch_millis = current_epoch_millis().unwrap();
        let expired = McpTaskRecoveryCheckpoint::encode(&expired_record).unwrap();
        assert!(matches!(
            second.prepare_task_recovery(binding, &expired, tools[0].id()),
            Err(McpClientError::TaskRecoveryRecordRejected)
        ));
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);

        let prepared = second
            .prepare_task_recovery(binding, &checkpoint, tools[0].id())
            .unwrap();
        let recovered = match second.recover_task(prepared).await.unwrap() {
            McpTaskPollOutcome::Pending(progress) => {
                assert_eq!(progress.state(), McpTaskState::InputRequired);
                progress.pending()
            },
            other => panic!("expected recovered input-required task, got {other:?}"),
        };
        assert_eq!(state.polls.load(Ordering::SeqCst), 1);
        assert!(second.pending_call_is_active(recovered).unwrap());
        assert!(!second.pending_call_is_active(old_pending).unwrap());
        let replacement = second.checkpoint_task(binding, recovered).unwrap();
        assert!(
            replacement.decode(binding).unwrap().revision
                > checkpoint.decode(binding).unwrap().revision
        );
        second.close().await.unwrap();
        second_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn recovery_checkpoint_survives_transient_failure_and_identity_attack() {
        let state = Arc::new(TaskFixtureState::default());
        let server = || TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        };
        let binding = task_recovery_binding("mcp://task-fixture/retry");
        let (mut first, first_server) = connect_task_fixture(server()).await;
        let pending = create_fixture_task(&mut first).await;
        let checkpoint = first.checkpoint_task(binding, pending).unwrap();
        first.close().await.unwrap();
        first_server.await.unwrap().unwrap();

        let (mut second, second_server) = connect_task_fixture(server()).await;
        let tools = second.discover_tools().await.unwrap();
        state.fail_next_poll.store(true, Ordering::SeqCst);
        let prepared = second
            .prepare_task_recovery(binding, &checkpoint, tools[0].id())
            .unwrap();
        assert_eq!(
            prepared
                .checkpoint()
                .decode(binding)
                .unwrap()
                .operation_count,
            checkpoint.decode(binding).unwrap().operation_count + 1
        );
        let replacement = prepared.checkpoint().as_bytes().to_vec();
        assert!(second.recover_task(prepared).await.is_err());
        assert_eq!(second.pending_call_count().unwrap(), 0);

        let checkpoint = McpTaskRecoveryCheckpoint::from_bytes(replacement).unwrap();
        state.wrong_identity.store(true, Ordering::SeqCst);
        let prepared = second
            .prepare_task_recovery(binding, &checkpoint, tools[0].id())
            .unwrap();
        assert_eq!(
            prepared
                .checkpoint()
                .decode(binding)
                .unwrap()
                .operation_count,
            checkpoint.decode(binding).unwrap().operation_count + 1
        );
        let replacement = prepared.checkpoint().as_bytes().to_vec();
        assert!(matches!(
            second.recover_task(prepared).await,
            Err(McpClientError::ResponseRejected(_))
        ));
        assert_eq!(second.pending_call_count().unwrap(), 0);

        let checkpoint = McpTaskRecoveryCheckpoint::from_bytes(replacement).unwrap();
        state.wrong_identity.store(false, Ordering::SeqCst);
        let prepared = second
            .prepare_task_recovery(binding, &checkpoint, tools[0].id())
            .unwrap();
        assert!(matches!(
            second.recover_task(prepared).await.unwrap(),
            McpTaskPollOutcome::Pending(_)
        ));
        assert_eq!(second.pending_call_count().unwrap(), 1);
        second.close().await.unwrap();
        second_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn terminal_recovery_returns_authority_without_retaining_a_handle() {
        let state = Arc::new(TaskFixtureState::default());
        let server = || TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        };
        let binding = task_recovery_binding("mcp://task-fixture/terminal");
        let (mut first, first_server) = connect_task_fixture(server()).await;
        let pending = create_fixture_task(&mut first).await;
        let checkpoint = first.checkpoint_task(binding, pending).unwrap();
        first.close().await.unwrap();
        first_server.await.unwrap().unwrap();

        state.cancelled.store(true, Ordering::SeqCst);
        let (mut second, second_server) = connect_task_fixture(server()).await;
        let tools = second.discover_tools().await.unwrap();
        let prepared = second
            .prepare_task_recovery(binding, &checkpoint, tools[0].id())
            .unwrap();
        assert!(matches!(
            second.recover_task(prepared).await.unwrap(),
            McpTaskPollOutcome::Cancelled
        ));
        assert_eq!(second.pending_call_count().unwrap(), 0);
        second.close().await.unwrap();
        second_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn replayed_checkpoint_has_one_local_retention_winner() {
        let state = Arc::new(TaskFixtureState::default());
        let server = || TaskFixtureServer {
            state: Arc::clone(&state),
            poll_interval_ms: 1,
            notify_after_create: false,
        };
        let binding = task_recovery_binding("mcp://task-fixture/replay");
        let (mut first, first_server) = connect_task_fixture(server()).await;
        let pending = create_fixture_task(&mut first).await;
        let checkpoint = first.checkpoint_task(binding, pending).unwrap();
        first.close().await.unwrap();
        first_server.await.unwrap().unwrap();

        let (mut second, second_server) = connect_task_fixture(server()).await;
        let tools = second.discover_tools().await.unwrap();
        let tool = tools[0].id();
        let left = second
            .prepare_task_recovery(binding, &checkpoint, tool)
            .unwrap();
        let right = second
            .prepare_task_recovery(binding, &checkpoint, tool)
            .unwrap();
        let (left, right) = tokio::join!(second.recover_task(left), second.recover_task(right),);
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        assert_eq!(second.pending_call_count().unwrap(), 1);
        second.close().await.unwrap();
        second_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn auto_lifecycle_prefers_stateless_2026_and_calls_discovered_tool() {
        let (mut client, server_task) = connect_fixture(FixtureServer).await;
        assert_eq!(
            client.connection_info().protocol_version,
            ProtocolVersion::V_2026_07_28.as_str()
        );

        let tools = client.discover_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].id.remote_name(), "echo");
        assert_eq!(tools[1].id.remote_name(), "reverse");

        let arguments: Map<String, Value> =
            serde_json::from_value(json!({"message": "hello"})).unwrap();
        let outcome = client.call_tool(&tools[0].id, arguments).await.unwrap();
        match outcome {
            McpToolCallOutcome::Complete(result) => {
                assert!(!result.is_error);
                assert_eq!(result.content[0]["text"], "hello");
            },
            other => panic!("unexpected tool outcome: {other:?}"),
        }
        assert_eq!(client.pending_call_count().unwrap(), 0);

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn initialization_advertises_only_the_exact_installed_presenters() {
        let configurations = [
            McpMrtrPresentationCapabilities::default(),
            McpMrtrPresentationCapabilities::new()
                .enable_sampling()
                .enable_form_elicitation()
                .enable_roots(),
        ];
        for presentation in configurations {
            let observed = Arc::new(Mutex::new(Vec::new()));
            let (mut client, server_task) = connect_fixture_with_presentation(
                CapabilityProbeServer {
                    observed: Arc::clone(&observed),
                },
                McpClientLimits::default(),
                Duration::from_secs(2),
                presentation,
            )
            .await;
            assert_eq!(client.mrtr_presentation_capabilities(), presentation);
            let tools = client.discover_tools().await.unwrap();
            assert!(matches!(
                client.call_tool(tools[0].id(), Map::new()).await.unwrap(),
                McpToolCallOutcome::Complete(_)
            ));
            assert_eq!(
                observed.lock().unwrap().as_slice(),
                &[presentation.sdk_client_capabilities()]
            );
            client.close().await.unwrap();
            server_task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn unsupported_or_unadvertised_input_shapes_fail_before_retention() {
        let none = McpMrtrPresentationCapabilities::default();
        assert_input_shape_rejected(
            input_required_with("sampling", [("sampling", base_sampling_input())]),
            none,
        )
        .await;
        assert_input_shape_rejected(
            input_required_with("form", [("form", form_elicitation_input())]),
            none,
        )
        .await;
        assert_input_shape_rejected(
            input_required_with("roots", [("roots", roots_input())]),
            none,
        )
        .await;

        let sampling = McpMrtrPresentationCapabilities::new().enable_sampling();
        assert_input_shape_rejected(
            input_required_with("context", [("sampling", contextual_sampling_input())]),
            sampling,
        )
        .await;
        assert_input_shape_rejected(
            input_required_with("tools", [("sampling", tool_sampling_input())]),
            sampling,
        )
        .await;
        assert_input_shape_rejected(
            input_required_with("tool-choice", [("sampling", tool_choice_sampling_input())]),
            sampling,
        )
        .await;

        let form = McpMrtrPresentationCapabilities::new().enable_form_elicitation();
        assert_input_shape_rejected(
            input_required_with("url", [("url", url_elicitation_input())]),
            form,
        )
        .await;

        let roots = McpMrtrPresentationCapabilities::new().enable_roots();
        assert_input_shape_rejected(
            input_required_with(
                "mixed",
                [
                    ("supported-roots", roots_input()),
                    ("unsupported-url", url_elicitation_input()),
                ],
            ),
            roots,
        )
        .await;
    }

    #[tokio::test]
    async fn unsupported_replacement_round_consumes_the_dispatched_revision_fail_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let presentation = McpMrtrPresentationCapabilities::new().enable_roots();
        let (mut client, server_task) = connect_fixture_with_presentation(
            SupportedThenUnsupportedServer {
                calls: Arc::clone(&calls),
            },
            McpClientLimits::default(),
            Duration::from_secs(2),
            presentation,
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) =
            client.call_tool(tools[0].id(), Map::new()).await.unwrap()
        else {
            panic!("expected supported roots round")
        };
        let slot = client.pending_input_slots(pending).unwrap()[0];
        let prepared = client
            .prepare_mrtr_responses(
                pending,
                vec![McpMrtrResponse::new(
                    slot.id(),
                    valid_mrtr_response(slot.kind()),
                )],
            )
            .unwrap();
        let error = client
            .resume_mrtr(client.claim_mrtr_responses(prepared).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            McpClientError::ContinuationCapabilityUnsupported
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(pending).unwrap());
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn governed_manual_driver_completes_state_sampling_form_and_roots_rounds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let presentation = McpMrtrPresentationCapabilities::new()
            .enable_sampling()
            .enable_form_elicitation()
            .enable_roots();
        let (mut client, server_task) = connect_fixture_with_presentation(
            QualifiedMultiRoundServer {
                calls: Arc::clone(&calls),
                requests: Arc::clone(&requests),
            },
            McpClientLimits::default(),
            Duration::from_secs(2),
            presentation,
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let arguments: Map<String, Value> =
            serde_json::from_value(json!({"message": "preserve-me"})).unwrap();
        let mut outcome = client
            .call_tool(tools[0].id(), arguments.clone())
            .await
            .unwrap();
        for expected_kind in [
            None,
            Some(crate::McpMrtrInputKind::Sampling),
            Some(crate::McpMrtrInputKind::Elicitation),
            Some(crate::McpMrtrInputKind::Roots),
        ] {
            let McpToolCallOutcome::InputRequired(pending) = outcome else {
                panic!("expected another input-required round")
            };
            let slots = client.pending_input_slots(pending).unwrap();
            assert_eq!(slots.first().map(|slot| slot.kind()), expected_kind);
            let responses = slots
                .iter()
                .map(|slot| McpMrtrResponse::new(slot.id(), valid_mrtr_response(slot.kind())))
                .collect();
            let prepared = client.prepare_mrtr_responses(pending, responses).unwrap();
            outcome = client
                .resume_mrtr(client.claim_mrtr_responses(prepared).unwrap())
                .await
                .unwrap();
        }
        let McpToolCallOutcome::Complete(result) = outcome else {
            panic!("expected terminal result")
        };
        assert_eq!(result.content[0]["text"], "qualified");
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert_eq!(client.pending_call_count().unwrap(), 0);

        {
            let observed = requests.lock().unwrap();
            assert_eq!(observed.len(), 5);
            assert!(observed
                .iter()
                .all(|request| request.arguments.as_ref() == Some(&arguments)));
            let expected_states = [
                None,
                Some("state-only"),
                Some("sampling-state"),
                Some("form-state"),
                Some("roots-state"),
            ];
            for (request, expected_state) in observed.iter().zip(expected_states) {
                assert_eq!(request.request_state.as_deref(), expected_state);
            }
            assert!(observed[0].input_responses.is_none());
            assert!(observed[1].input_responses.is_none());
            for (request, key) in
                observed[2..]
                    .iter()
                    .zip(["private-sampling", "private-form", "private-roots"])
            {
                let responses = request.input_responses.as_ref().unwrap();
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key(key));
            }
        }

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn concurrent_mrtr_calls_keep_state_keys_responses_and_results_isolated() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let maximum = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture_with_presentation(
            ConcurrentMrtrServer {
                requests: Arc::clone(&requests),
                active: Arc::new(AtomicUsize::new(0)),
                maximum: Arc::clone(&maximum),
            },
            McpClientLimits::default(),
            Duration::from_secs(2),
            McpMrtrPresentationCapabilities::new().enable_roots(),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let lane_a = serde_json::from_value(json!({"lane": "a"})).unwrap();
        let lane_b = serde_json::from_value(json!({"lane": "b"})).unwrap();
        let (first, second) = tokio::join!(
            client.call_tool(tools[0].id(), lane_a),
            client.call_tool(tools[0].id(), lane_b),
        );
        let McpToolCallOutcome::InputRequired(first) = first.unwrap() else {
            panic!("expected first roots continuation")
        };
        let McpToolCallOutcome::InputRequired(second) = second.unwrap() else {
            panic!("expected second roots continuation")
        };
        assert_ne!(first, second);

        let prepare = |pending| {
            let slot = client.pending_input_slots(pending).unwrap()[0];
            client
                .prepare_mrtr_responses(
                    pending,
                    vec![McpMrtrResponse::new(
                        slot.id(),
                        valid_mrtr_response(slot.kind()),
                    )],
                )
                .unwrap()
        };
        let first_claim = client.claim_mrtr_responses(prepare(first)).unwrap();
        let second_claim = client.claim_mrtr_responses(prepare(second)).unwrap();
        let (first_result, second_result) = tokio::join!(
            client.resume_mrtr(first_claim),
            client.resume_mrtr(second_claim),
        );
        let mut texts = [first_result, second_result]
            .into_iter()
            .map(|outcome| match outcome.unwrap() {
                McpToolCallOutcome::Complete(result) => {
                    result.content[0]["text"].as_str().unwrap().to_owned()
                },
                other => panic!("unexpected concurrent outcome: {other:?}"),
            })
            .collect::<Vec<_>>();
        texts.sort();
        assert_eq!(texts, ["a", "b"]);
        assert!(maximum.load(Ordering::SeqCst) > 1);
        assert_eq!(client.pending_call_count().unwrap(), 0);

        {
            let observed = requests.lock().unwrap();
            assert_eq!(observed.len(), 4);
            for request in observed.iter() {
                let lane = request.arguments.as_ref().unwrap()["lane"]
                    .as_str()
                    .unwrap();
                if request.request_state.is_none() {
                    assert!(request.input_responses.is_none());
                } else {
                    let expected_state = format!("state-{lane}");
                    assert_eq!(
                        request.request_state.as_deref(),
                        Some(expected_state.as_str())
                    );
                    let responses = request.input_responses.as_ref().unwrap();
                    assert_eq!(responses.len(), 1);
                    assert!(responses.contains_key(&format!("private-root-{lane}")));
                }
            }
        }

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn incomplete_calls_are_retained_opaquely_and_capacity_blocks_before_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let limits = McpClientLimits {
            max_pending_continuations: 1,
            ..McpClientLimits::default()
        };
        let (mut client, server_task) = connect_fixture_with(
            InputRequiredServer {
                calls: Arc::clone(&calls),
            },
            limits,
            Duration::from_secs(2),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();

        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected an input-required continuation")
        };
        assert!(client.pending_call_is_active(pending).unwrap());
        assert_eq!(client.pending_call_count().unwrap(), 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let rendered = format!("{pending:?}");
        assert!(!rendered.contains("canary-provider-request-state"));
        assert!(!rendered.contains("needs-input"));

        let error = client
            .call_tool(tools[0].id(), Map::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            McpClientError::ContinuationCapacityExceeded
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn atomic_claim_is_transport_dormant_and_drop_restores_the_revision() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture(InputRequiredServer {
            calls: Arc::clone(&calls),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected an input-required continuation")
        };

        let prepared = client.prepare_mrtr_responses(pending, Vec::new()).unwrap();
        let claim = client.claim_mrtr_responses(prepared).unwrap();
        assert_eq!(claim.pending(), pending);
        assert!(!client.pending_call_is_active(pending).unwrap());
        assert_eq!(client.pending_call_count().unwrap(), 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        drop(claim);
        assert!(client.pending_call_is_active(pending).unwrap());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn official_sdk_retry_advances_exact_revisions_and_preserves_original_arguments() {
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (mut client, server_task) = connect_fixture(MultiRoundInputServer {
            calls: Arc::clone(&calls),
            requests: Arc::clone(&requests),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let arguments: Map<String, Value> =
            serde_json::from_value(json!({"message": "canary-original-argument"})).unwrap();
        let outcome = client
            .call_tool(tools[0].id(), arguments.clone())
            .await
            .unwrap();
        let McpToolCallOutcome::InputRequired(first) = outcome else {
            panic!("expected first input-required revision")
        };
        assert_eq!(first.revision().get(), 1);

        let first_claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(first, Vec::new()).unwrap())
            .unwrap();
        let outcome = client.resume_mrtr(first_claim).await.unwrap();
        let McpToolCallOutcome::InputRequired(second) = outcome else {
            panic!("expected second input-required revision")
        };
        assert_eq!(second.revision().get(), 2);
        assert!(!client.pending_call_is_active(first).unwrap());
        assert!(client.pending_call_is_active(second).unwrap());

        let second_claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(second, Vec::new()).unwrap())
            .unwrap();
        let outcome = client.resume_mrtr(second_claim).await.unwrap();
        let McpToolCallOutcome::Complete(result) = outcome else {
            panic!("expected terminal result")
        };
        assert_eq!(result.content[0]["text"], "finished");
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        {
            let observed = requests.lock().unwrap();
            assert_eq!(observed.len(), 3);
            assert!(observed
                .iter()
                .all(|request| request.arguments.as_ref() == Some(&arguments)));
            assert_eq!(observed[0].request_state, None);
            assert_eq!(
                observed[1].request_state.as_deref(),
                Some("canary-round-one")
            );
            assert_eq!(
                observed[2].request_state.as_deref(),
                Some("canary-round-two")
            );
            assert!(observed
                .iter()
                .all(|request| request.input_responses.is_none()));
        }

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn official_sdk_retry_binds_typed_response_to_the_private_server_key() {
        let calls = Arc::new(AtomicUsize::new(0));
        let retry = Arc::new(Mutex::new(None));
        let (mut client, server_task) = connect_fixture_with_presentation(
            RootsInputServer {
                calls: Arc::clone(&calls),
                retry: Arc::clone(&retry),
            },
            McpClientLimits::default(),
            Duration::from_secs(2),
            McpMrtrPresentationCapabilities::new().enable_roots(),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected roots input revision")
        };
        let slots = client.pending_input_slots(pending).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].kind(), crate::McpMrtrInputKind::Roots);
        let roots =
            serde_json::to_value(ListRootsResult::new(vec![Root::new("file:///canary/root")]))
                .unwrap();
        let prepared = client
            .prepare_mrtr_responses(pending, vec![McpMrtrResponse::new(slots[0].id(), roots)])
            .unwrap();
        let claim = client.claim_mrtr_responses(prepared).unwrap();
        let outcome = client.resume_mrtr(claim).await.unwrap();
        assert!(matches!(outcome, McpToolCallOutcome::Complete(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);

        {
            let retry = retry.lock().unwrap();
            let request = retry.as_ref().unwrap();
            assert_eq!(request.request_state.as_deref(), Some("canary-root-state"));
            let responses = request.input_responses.as_ref().unwrap();
            assert_eq!(responses.len(), 1);
            assert!(responses.contains_key("canary-private-root-key"));
        }

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_sdk_retry_is_redacted_and_cannot_replay_the_old_revision() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture(FailingResumeServer {
            calls: Arc::clone(&calls),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let error = client.resume_mrtr(claim).await.unwrap_err();
        let rendered = error.to_string();
        assert!(matches!(error, McpClientError::Call(_)));
        assert!(!rendered.contains("canary-retry-secret-message"));
        assert!(!rendered.contains("canary-retry-secret-data"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(pending).unwrap());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn timed_out_sdk_retry_settles_local_continuation_fail_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (mut client, server_task) = connect_fixture_with(
            BlockingResumeServer {
                calls: Arc::clone(&calls),
                started,
                cancelled: Arc::clone(&cancelled),
            },
            McpClientLimits::default(),
            Duration::from_millis(40),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let error = client.resume_mrtr(claim).await.unwrap_err();
        assert!(matches!(error, McpClientError::CallTimeout));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        wait_for_flag(&cancelled).await;

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dropping_sdk_retry_future_releases_local_continuation_without_replay() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (mut client, server_task) = connect_fixture(BlockingResumeServer {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let mut resume = Box::pin(client.resume_mrtr(claim));
        tokio::select! {
            result = &mut resume => panic!("blocking retry completed unexpectedly: {result:?}"),
            () = started.notified() => {},
        }
        drop(resume);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        wait_for_flag(&cancelled).await;

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn explicit_sdk_cancellation_after_dispatch_consumes_the_revision() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (mut client, server_task) = connect_fixture(BlockingResumeServer {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let cancellation = McpCallCancellation::new();
        let mut resume = Box::pin(client.resume_mrtr_with_cancellation(claim, &cancellation));
        tokio::select! {
            result = &mut resume => panic!("blocking retry completed unexpectedly: {result:?}"),
            () = started.notified() => {},
        }
        cancellation.cancel();
        let error = resume.await.unwrap_err();
        assert!(matches!(error, McpClientError::CallCancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(pending).unwrap());
        wait_for_flag(&cancelled).await;

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn pre_cancelled_sdk_retry_restores_the_undispatched_revision() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut client, server_task) = connect_fixture(InputRequiredServer {
            calls: Arc::clone(&calls),
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let cancellation = McpCallCancellation::new();
        cancellation.cancel();
        let error = client
            .resume_mrtr_with_cancellation(claim, &cancellation)
            .await
            .unwrap_err();
        assert!(matches!(error, McpClientError::CallCancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(client.pending_call_count().unwrap(), 1);
        assert!(client.pending_call_is_active(pending).unwrap());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn transport_loss_during_sdk_retry_consumes_the_dispatched_revision() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (mut client, server_task) = connect_fixture(BlockingResumeServer {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            cancelled,
        })
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(pending) = outcome else {
            panic!("expected input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(pending, Vec::new()).unwrap())
            .unwrap();
        let mut resume = Box::pin(client.resume_mrtr(claim));
        tokio::select! {
            result = &mut resume => panic!("blocking retry completed unexpectedly: {result:?}"),
            () = started.notified() => {},
        }
        server_task.abort();
        let error = resume.await.unwrap_err();
        assert!(matches!(error, McpClientError::Call(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(pending).unwrap());
        assert!(server_task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn state_only_sdk_round_budget_is_exact_and_fails_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let limits = McpClientLimits {
            max_mrtr_state_only_rounds: 2,
            ..McpClientLimits::default()
        };
        let (mut client, server_task) = connect_fixture_with(
            InputRequiredServer {
                calls: Arc::clone(&calls),
            },
            limits,
            Duration::from_secs(2),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(first) = outcome else {
            panic!("expected first input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(first, Vec::new()).unwrap())
            .unwrap();
        let outcome = client.resume_mrtr(claim).await.unwrap();
        let McpToolCallOutcome::InputRequired(second) = outcome else {
            panic!("expected second input-required revision")
        };
        let claim = client
            .claim_mrtr_responses(client.prepare_mrtr_responses(second, Vec::new()).unwrap())
            .unwrap();
        let error = client.resume_mrtr(claim).await.unwrap_err();
        assert!(matches!(
            error,
            McpClientError::ContinuationRoundLimitExceeded
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(second).unwrap());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn interactive_sdk_round_budget_is_exact_and_fails_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let limits = McpClientLimits {
            max_mrtr_interactive_rounds: 2,
            ..McpClientLimits::default()
        };
        let (mut client, server_task) = connect_fixture_with_presentation(
            InteractiveInputServer {
                calls: Arc::clone(&calls),
            },
            limits,
            Duration::from_secs(2),
            McpMrtrPresentationCapabilities::new().enable_roots(),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let outcome = client.call_tool(tools[0].id(), Map::new()).await.unwrap();
        let McpToolCallOutcome::InputRequired(mut pending) = outcome else {
            panic!("expected first interactive revision")
        };
        for expected_revision in 2..=3 {
            let slots = client.pending_input_slots(pending).unwrap();
            assert_eq!(slots.len(), 1);
            let roots =
                serde_json::to_value(ListRootsResult::new(vec![Root::new("file:///canary/root")]))
                    .unwrap();
            let prepared = client
                .prepare_mrtr_responses(pending, vec![McpMrtrResponse::new(slots[0].id(), roots)])
                .unwrap();
            let claim = client.claim_mrtr_responses(prepared).unwrap();
            if expected_revision == 2 {
                let outcome = client.resume_mrtr(claim).await.unwrap();
                let McpToolCallOutcome::InputRequired(next) = outcome else {
                    panic!("expected second interactive revision")
                };
                pending = next;
            } else {
                let error = client.resume_mrtr(claim).await.unwrap_err();
                assert!(matches!(
                    error,
                    McpClientError::ContinuationRoundLimitExceeded
                ));
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(client.pending_call_count().unwrap(), 0);
        assert!(!client.pending_call_is_active(pending).unwrap());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[test]
    fn cancellation_authority_is_payload_free_and_non_serializable() {
        static_assertions::assert_not_impl_any!(
            McpCallCancellation: serde::Serialize, serde::de::DeserializeOwned
        );
        let cancellation = McpCallCancellation::new();
        assert_eq!(
            format!("{cancellation:?}"),
            "McpCallCancellation { cancelled: false }"
        );
        cancellation.cancel();
        assert!(cancellation.is_cancelled());
        assert_eq!(
            format!("{cancellation:?}"),
            "McpCallCancellation { cancelled: true }"
        );
    }

    #[tokio::test]
    async fn call_rejects_ids_not_minted_by_discovery() {
        let (client, server_task) = connect_fixture(FixtureServer).await;
        let error = client
            .call_tool(
                &McpToolId::new("echo".to_owned(), u64::MAX, u64::MAX),
                Map::new(),
            )
            .await
            .expect_err("undiscovered calls must fail closed");
        assert!(matches!(error, McpClientError::ToolNotDiscovered));
        assert!(!error.to_string().contains("echo"));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rediscovery_revokes_every_identifier_from_the_previous_generation() {
        let (mut client, server_task) = connect_fixture(FixtureServer).await;
        let first = client.discover_tools().await.unwrap();
        let second = client.discover_tools().await.unwrap();

        let error = client
            .call_tool(&first[0].id, Map::new())
            .await
            .expect_err("the previous discovery generation must be revoked");
        assert!(matches!(error, McpClientError::ToolNotDiscovered));
        assert!(client.call_tool(&second[0].id, Map::new()).await.is_ok());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn failed_rediscovery_preserves_the_previous_callable_generation() {
        let list_calls = Arc::new(AtomicUsize::new(0));
        let server = FailingRediscoveryServer {
            list_calls: Arc::clone(&list_calls),
        };
        let (mut client, server_task) = connect_fixture(server).await;
        let stable = client.discover_tools().await.unwrap();

        let error = client.discover_tools().await.unwrap_err();
        assert!(matches!(error, McpClientError::Discovery(_)));
        assert!(!error.to_string().contains("rediscovery-secret-canary"));
        assert!(client.call_tool(stable[0].id(), Map::new()).await.is_ok());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_rediscovery_preserves_authority_and_notifies_the_server() {
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let server = BlockingRediscoveryServer {
            list_calls: Arc::new(AtomicUsize::new(0)),
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        };
        let (mut client, server_task) = connect_fixture(server).await;
        let stable = client.discover_tools().await.unwrap();

        let mut rediscovery = Box::pin(client.discover_tools());
        tokio::select! {
            result = &mut rediscovery => panic!("blocking rediscovery completed unexpectedly: {result:?}"),
            () = started.notified() => {},
        }
        drop(rediscovery);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelled.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(client.call_tool(stable[0].id(), Map::new()).await.is_ok());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn exhausted_discovery_generation_preserves_the_previous_authority() {
        let (mut client, server_task) = connect_fixture(FixtureServer).await;
        let stable = client.discover_tools().await.unwrap();
        client.discovery_generation = u64::MAX;

        let error = client.discover_tools().await.unwrap_err();
        assert!(matches!(error, McpClientError::CatalogRejected(_)));
        assert!(client.call_tool(stable[0].id(), Map::new()).await.is_ok());

        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn identifiers_are_bound_to_the_exact_client_instance() {
        let (mut first_client, first_server) = connect_fixture(FixtureServer).await;
        let (mut second_client, second_server) = connect_fixture(FixtureServer).await;
        let first_tools = first_client.discover_tools().await.unwrap();
        let second_tools = second_client.discover_tools().await.unwrap();

        let error = second_client
            .call_tool(&first_tools[0].id, Map::new())
            .await
            .expect_err("another client instance must not accept this identifier");
        assert!(matches!(error, McpClientError::ToolNotDiscovered));
        assert!(second_client
            .call_tool(&second_tools[0].id, Map::new())
            .await
            .is_ok());

        first_client.close().await.unwrap();
        second_client.close().await.unwrap();
        first_server.await.unwrap().unwrap();
        second_server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn public_call_boundary_iteratively_drops_rejected_deep_arguments() {
        let (mut client, server_task) = connect_fixture(FixtureServer).await;
        let tools = client.discover_tools().await.unwrap();
        let mut nested = Value::Null;
        for _ in 0..20_000 {
            nested = Value::Array(vec![nested]);
        }
        let mut arguments = Map::new();
        arguments.insert("deep".to_owned(), nested);

        let error = client.call_tool(&tools[0].id, arguments).await.unwrap_err();
        assert!(matches!(error, McpClientError::RequestRejected(_)));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn auto_lifecycle_falls_back_only_to_declared_legacy_version() {
        let (client, server_task) = connect_fixture(LegacyFixtureServer).await;
        assert_eq!(
            client.connection_info().protocol_version,
            ProtocolVersion::V_2025_11_25.as_str()
        );
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn discovery_rejects_repeated_and_endless_cursors() {
        let (mut repeated, repeated_task) = connect_fixture(RepeatingCursorServer).await;
        let error = repeated.discover_tools().await.unwrap_err();
        assert!(error.to_string().contains("repeated cursor"));
        repeated.close().await.unwrap();
        repeated_task.await.unwrap().unwrap();

        let limits = McpClientLimits {
            max_tool_pages: 3,
            ..Default::default()
        };
        let (mut endless, endless_task) =
            connect_fixture_with(EndlessCursorServer, limits, Duration::from_secs(2)).await;
        let error = endless.discover_tools().await.unwrap_err();
        assert!(error.to_string().contains("exceeded 3 pages"));
        endless.close().await.unwrap();
        endless_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn remote_error_messages_and_data_are_not_exposed() {
        let (mut client, server_task) = connect_fixture(FailingToolServer).await;
        let tools = client.discover_tools().await.unwrap();
        let error = client
            .call_tool(&tools[0].id, Map::new())
            .await
            .unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("-32603"));
        assert!(!rendered.contains("canary-secret-message"));
        assert!(!rendered.contains("canary-secret-data"));
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_timeout_notifies_server_cancellation() {
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let server = BlockingToolServer {
            started,
            cancelled: Arc::clone(&cancelled),
        };
        let (mut client, server_task) = connect_fixture_with(
            server,
            McpClientLimits::default(),
            Duration::from_millis(40),
        )
        .await;
        let tools = client.discover_tools().await.unwrap();
        let error = client
            .call_tool(&tools[0].id, Map::new())
            .await
            .unwrap_err();
        assert!(matches!(error, McpClientError::CallTimeout));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelled.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(client.pending_call_count().unwrap(), 0);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dropping_request_future_notifies_server_cancellation() {
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let server = BlockingToolServer {
            started: Arc::clone(&started),
            cancelled: Arc::clone(&cancelled),
        };
        let (mut client, server_task) = connect_fixture(server).await;
        let tools = client.discover_tools().await.unwrap();
        let mut call = Box::pin(client.call_tool(&tools[0].id, Map::new()));
        tokio::select! {
            result = &mut call => panic!("blocking call completed unexpectedly: {result:?}"),
            () = started.notified() => {},
        }
        drop(call);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelled.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(client.pending_call_count().unwrap(), 0);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn concurrent_calls_keep_independent_request_lifecycles() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let server = ConcurrentToolServer {
            active,
            maximum: Arc::clone(&maximum),
        };
        // This test exercises transport/request independence, not the separate
        // continuation-capacity boundary. Keep the configured worst-case result
        // reservation small enough for all 24 calls to be admitted concurrently.
        let limits = McpClientLimits {
            max_result_bytes: 64 * 1024,
            ..McpClientLimits::default()
        };
        let (mut client, server_task) =
            connect_fixture_with(server, limits, Duration::from_secs(2)).await;
        let tools = client.discover_tools().await.unwrap();
        let outcomes = futures_util::future::join_all(
            (0..24).map(|_| client.call_tool(&tools[0].id, Map::new())),
        )
        .await;
        assert!(outcomes.iter().all(Result::is_ok));
        assert!(maximum.load(Ordering::SeqCst) > 1);
        client.close().await.unwrap();
        server_task.await.unwrap().unwrap();
    }
}
