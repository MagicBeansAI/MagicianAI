use std::{collections::BTreeMap, ffi::OsString, fmt, path::PathBuf, time::Duration};

use zeroize::{Zeroize, ZeroizeOnDrop};

use tokio::sync::mpsc;

use crate::McpClientError;

const HARD_MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const HARD_MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HARD_MAX_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);
const HARD_MAX_CONTINUATION_TIMEOUT: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const HARD_MAX_TOOL_COUNT: usize = 4_096;
const HARD_MAX_TOOL_PAGES: usize = 512;
const HARD_MAX_CURSOR_BYTES: usize = 8 * 1_024;
const HARD_MAX_TOOL_NAME_BYTES: usize = 1_024;
const HARD_MAX_TOOL_TITLE_BYTES: usize = 8 * 1_024;
const HARD_MAX_TOOL_DESCRIPTION_BYTES: usize = 256 * 1_024;
const HARD_MAX_SCHEMA_BYTES: usize = 8 * 1_024 * 1_024;
const HARD_MAX_CATALOG_BYTES: usize = 32 * 1_024 * 1_024;
const HARD_MAX_REQUEST_BYTES: usize = 16 * 1_024 * 1_024;
const HARD_MAX_RESULT_BYTES: usize = 32 * 1_024 * 1_024;
const HARD_MAX_JSON_DEPTH: usize = 64;
const HARD_MAX_JSON_NODES: usize = 1_000_000;
const HARD_MAX_SSE_EVENT_BYTES: usize = 32 * 1_024 * 1_024;
const HARD_MAX_TRANSPORT_MESSAGE_BYTES: usize = 40 * 1_024 * 1_024;
const HARD_MAX_PENDING_CONTINUATIONS: usize = 1_024;
const HARD_MAX_CONTINUATION_BYTES: usize = 1_024 * 1_024 * 1_024;
const HARD_MAX_MRTR_INPUTS: usize = 256;
const HARD_MAX_MRTR_STATE_ONLY_ROUNDS: usize = 64;
const HARD_MAX_MRTR_INTERACTIVE_ROUNDS: usize = 64;
const HARD_MAX_TASK_OPERATIONS: usize = 1_000_000;
const HARD_MAX_TASK_ID_BYTES: usize = 64 * 1_024;
const HARD_MAX_TASK_STATUS_MESSAGE_BYTES: usize = 256 * 1_024;
const HARD_MAX_TASK_POLL_FLOOR: Duration = Duration::from_secs(60);
const HARD_MAX_RESOURCE_SUBSCRIPTIONS: usize = 1_024;
const HARD_MAX_RESOURCE_URI_BYTES: usize = 64 * 1_024;
const HARD_MAX_RESOURCE_SUBSCRIPTION_BYTES: usize = 8 * 1_024 * 1_024;
const HARD_MAX_SUBSCRIPTION_CHANNEL_CAPACITY: usize = 4_096;

/// Bearer credential supplied by the Auth Broker for one connection.
///
/// It has no `Clone`, serialization, or value-bearing `Debug` implementation.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

pub struct StdioTransportConfig {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub working_directory: Option<PathBuf>,
}

impl StdioTransportConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            working_directory: None,
        }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_environment(
        mut self,
        environment: impl IntoIterator<Item = (impl Into<OsString>, impl Into<OsString>)>,
    ) -> Self {
        self.environment = environment
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    pub fn with_working_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(path.into());
        self
    }
}

impl fmt::Debug for StdioTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let environment_names: Vec<_> = self.environment.keys().collect();
        formatter
            .debug_struct("StdioTransportConfig")
            .field("executable", &self.executable)
            .field("arg_count", &self.args.len())
            .field("environment_names", &environment_names)
            .field("working_directory", &self.working_directory)
            .finish()
    }
}

pub struct StreamableHttpTransportConfig {
    pub endpoint: String,
    pub bearer_token: Option<BearerToken>,
    /// Plain HTTP is accepted only for loopback development endpoints.
    pub allow_loopback_http: bool,
}

impl StreamableHttpTransportConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token: None,
            allow_loopback_http: true,
        }
    }

    pub fn with_bearer_token(mut self, bearer_token: BearerToken) -> Self {
        self.bearer_token = Some(bearer_token);
        self
    }
}

impl fmt::Debug for StreamableHttpTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted_endpoint = url::Url::parse(&self.endpoint)
            .map(|mut endpoint| {
                let _ = endpoint.set_username("");
                let _ = endpoint.set_password(None);
                endpoint.set_path("/");
                endpoint.set_query(None);
                endpoint.set_fragment(None);
                endpoint.to_string()
            })
            .unwrap_or_else(|_| "[INVALID ENDPOINT]".to_owned());
        formatter
            .debug_struct("StreamableHttpTransportConfig")
            .field("endpoint", &redacted_endpoint)
            .field("has_bearer_token", &self.bearer_token.is_some())
            .field("allow_loopback_http", &self.allow_loopback_http)
            .finish()
    }
}

/// One already-authenticated bidirectional text channel carrying MCP JSON-RPC.
///
/// The embedding runtime owns the socket, identity, and reconnect lifecycle.
/// Keeping those concerns outside this type lets the governed client retain
/// protocol ownership without learning about a product-specific device route.
pub struct DuplexJsonTransportConfig {
    pub(crate) inbound: mpsc::Receiver<String>,
    pub(crate) outbound: mpsc::Sender<String>,
}

impl DuplexJsonTransportConfig {
    pub fn new(inbound: mpsc::Receiver<String>, outbound: mpsc::Sender<String>) -> Self {
        Self { inbound, outbound }
    }
}

impl fmt::Debug for DuplexJsonTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DuplexJsonTransportConfig")
            .field("inbound_capacity", &self.inbound.capacity())
            .field("outbound_capacity", &self.outbound.capacity())
            .finish()
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum McpTransportConfig {
    Stdio(StdioTransportConfig),
    StreamableHttp(StreamableHttpTransportConfig),
    DuplexJson(DuplexJsonTransportConfig),
}

#[derive(Debug, Clone)]
pub struct McpClientLimits {
    pub max_tool_count: usize,
    pub max_tool_pages: usize,
    pub max_cursor_bytes: usize,
    pub max_tool_name_bytes: usize,
    pub max_tool_title_bytes: usize,
    pub max_tool_description_bytes: usize,
    pub max_schema_bytes: usize,
    pub max_catalog_bytes: usize,
    pub max_request_bytes: usize,
    pub max_result_bytes: usize,
    pub max_json_depth: usize,
    pub max_json_nodes: usize,
    pub max_sse_event_bytes: usize,
    /// Maximum raw stdio line, HTTP body, or duplex JSON frame accepted before decoding.
    pub max_transport_message_bytes: usize,
    /// Maximum in-flight plus retained MRTR/task continuations for one client session.
    pub max_pending_continuations: usize,
    /// Aggregate reservation budget for retained request and pending-result authority.
    pub max_continuation_bytes: usize,
    /// Maximum SDK-issued response slots accepted in one MRTR round.
    pub max_mrtr_inputs: usize,
    /// Maximum input-required rounds with no SDK-issued response slots.
    pub max_mrtr_state_only_rounds: usize,
    /// Maximum input-required rounds containing one or more SDK-issued response slots.
    pub max_mrtr_interactive_rounds: usize,
    /// Maximum official task operations (`get`, `update`, and `cancel`) per task.
    pub max_task_operations: usize,
    /// Maximum retained server task identifier length.
    pub max_task_id_bytes: usize,
    /// Maximum accepted task status-message length. Messages remain provider-private.
    pub max_task_status_message_bytes: usize,
    /// Maximum exact resource URIs in one governed notification subscription.
    pub max_resource_subscriptions: usize,
    /// Maximum byte length of one retained resource URI.
    pub max_resource_uri_bytes: usize,
    /// Aggregate byte budget for retained resource subscription URIs.
    pub max_resource_subscription_bytes: usize,
}

impl Default for McpClientLimits {
    fn default() -> Self {
        Self {
            max_tool_count: 512,
            max_tool_pages: 128,
            max_cursor_bytes: 1_024,
            max_tool_name_bytes: 256,
            max_tool_title_bytes: 1_024,
            max_tool_description_bytes: 32 * 1_024,
            max_schema_bytes: 1024 * 1024,
            max_catalog_bytes: 8 * 1024 * 1024,
            max_request_bytes: 4 * 1024 * 1024,
            max_result_bytes: 16 * 1024 * 1024,
            max_json_depth: 64,
            max_json_nodes: 100_000,
            max_sse_event_bytes: 4 * 1024 * 1024,
            max_transport_message_bytes: 20 * 1024 * 1024,
            max_pending_continuations: 64,
            max_continuation_bytes: 64 * 1024 * 1024,
            max_mrtr_inputs: 64,
            max_mrtr_state_only_rounds: 16,
            max_mrtr_interactive_rounds: 16,
            max_task_operations: 65_536,
            max_task_id_bytes: 1_024,
            max_task_status_message_bytes: 16 * 1_024,
            max_resource_subscriptions: 64,
            max_resource_uri_bytes: 8 * 1_024,
            max_resource_subscription_bytes: 256 * 1_024,
        }
    }
}

/// Exact governed support for the official MCP Tasks extension.
///
/// The default is disabled. Enabling this capability is an immutable connection-time
/// decision: the client advertises `io.modelcontextprotocol/tasks` and accepts task
/// results only when the peer advertises the same extension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpTaskLifecycleCapabilities {
    enabled: bool,
}

impl McpTaskLifecycleCapabilities {
    pub const fn new() -> Self {
        Self { enabled: false }
    }

    pub const fn enable(mut self) -> Self {
        self.enabled = true;
        self
    }

    pub const fn enabled(self) -> bool {
        self.enabled
    }

    pub(crate) fn apply_to(self, capabilities: &mut rmcp::model::ClientCapabilities) {
        if self.enabled {
            capabilities
                .extensions
                .get_or_insert_with(rmcp::model::ExtensionCapabilities::new)
                .insert(
                    rmcp::model::TASKS_EXTENSION_ID.to_owned(),
                    rmcp::model::JsonObject::new(),
                );
        }
    }
}

/// Exact notification categories installed by the trusted caller.
///
/// The default is empty. These flags do not claim server support: the client intersects
/// them with the initialized peer capabilities and requires every opened subscription to
/// be acknowledged exactly before it becomes authoritative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpSubscriptionCapabilities {
    tools_list_changed: bool,
    prompts_list_changed: bool,
    resources_list_changed: bool,
    resource_updates: bool,
}

impl McpSubscriptionCapabilities {
    pub const fn new() -> Self {
        Self {
            tools_list_changed: false,
            prompts_list_changed: false,
            resources_list_changed: false,
            resource_updates: false,
        }
    }

    pub const fn enable_tools_list_changed(mut self) -> Self {
        self.tools_list_changed = true;
        self
    }

    pub const fn enable_prompts_list_changed(mut self) -> Self {
        self.prompts_list_changed = true;
        self
    }

    pub const fn enable_resources_list_changed(mut self) -> Self {
        self.resources_list_changed = true;
        self
    }

    pub const fn enable_resource_updates(mut self) -> Self {
        self.resource_updates = true;
        self
    }

    pub const fn tools_list_changed(self) -> bool {
        self.tools_list_changed
    }

    pub const fn prompts_list_changed(self) -> bool {
        self.prompts_list_changed
    }

    pub const fn resources_list_changed(self) -> bool {
        self.resources_list_changed
    }

    pub const fn resource_updates(self) -> bool {
        self.resource_updates
    }

    pub const fn is_empty(self) -> bool {
        !self.tools_list_changed
            && !self.prompts_list_changed
            && !self.resources_list_changed
            && !self.resource_updates
    }

    pub(crate) fn supported_by(self, capabilities: &rmcp::model::ServerCapabilities) -> Self {
        Self {
            tools_list_changed: self.tools_list_changed
                && capabilities
                    .tools
                    .as_ref()
                    .is_some_and(|value| value.list_changed == Some(true)),
            prompts_list_changed: self.prompts_list_changed
                && capabilities
                    .prompts
                    .as_ref()
                    .is_some_and(|value| value.list_changed == Some(true)),
            resources_list_changed: self.resources_list_changed
                && capabilities
                    .resources
                    .as_ref()
                    .is_some_and(|value| value.list_changed == Some(true)),
            resource_updates: self.resource_updates
                && capabilities
                    .resources
                    .as_ref()
                    .is_some_and(|value| value.subscribe == Some(true)),
        }
    }
}

/// Product presentation handlers installed for MCP multi-round input requests.
///
/// The default is empty and advertises no MRTR input capabilities. A caller may enable
/// a kind only after installing the corresponding governed product handler. Sampling is
/// intentionally limited to the base capability (no tools or context inclusion), and
/// elicitation is intentionally limited to schema-validated form mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpMrtrPresentationCapabilities {
    sampling: bool,
    form_elicitation: bool,
    roots: bool,
}

impl McpMrtrPresentationCapabilities {
    pub const fn new() -> Self {
        Self {
            sampling: false,
            form_elicitation: false,
            roots: false,
        }
    }

    pub const fn enable_sampling(mut self) -> Self {
        self.sampling = true;
        self
    }

    pub const fn enable_form_elicitation(mut self) -> Self {
        self.form_elicitation = true;
        self
    }

    pub const fn enable_roots(mut self) -> Self {
        self.roots = true;
        self
    }

    pub const fn sampling(self) -> bool {
        self.sampling
    }

    pub const fn form_elicitation(self) -> bool {
        self.form_elicitation
    }

    pub const fn roots(self) -> bool {
        self.roots
    }

    #[allow(deprecated)]
    pub(crate) fn sdk_client_capabilities(self) -> rmcp::model::ClientCapabilities {
        use rmcp::model::{
            ClientCapabilities, ElicitationCapability, FormElicitationCapability,
            RootsCapabilities, SamplingCapability,
        };

        let mut capabilities = ClientCapabilities::default();
        if self.sampling {
            capabilities.sampling = Some(SamplingCapability::default());
        }
        if self.form_elicitation {
            capabilities.elicitation = Some(
                ElicitationCapability::new()
                    .with_form(FormElicitationCapability::new().with_schema_validation(true)),
            );
        }
        if self.roots {
            capabilities.roots = Some(RootsCapabilities::default());
        }
        capabilities
    }

    #[allow(deprecated)]
    pub(crate) fn supports_input_request(self, request: &rmcp::model::InputRequest) -> bool {
        use rmcp::model::{ElicitRequestParams, InputRequest};

        match request {
            InputRequest::CreateMessage(request) => {
                self.sampling
                    && request.params.tools.is_none()
                    && request.params.tool_choice.is_none()
                    && request.params.include_context.is_none()
            },
            InputRequest::Elicitation(request) => {
                self.form_elicitation
                    && matches!(
                        &request.params,
                        ElicitRequestParams::FormElicitationParams { .. }
                    )
            },
            InputRequest::ListRoots(_) => self.roots,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct McpClientConfig {
    pub transport: McpTransportConfig,
    pub limits: McpClientLimits,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    /// Maximum local retention of an incomplete MRTR or task continuation.
    pub continuation_timeout: Duration,
    /// Exact governed MRTR presentation handlers installed by the trusted caller.
    pub mrtr_presentation_capabilities: McpMrtrPresentationCapabilities,
    /// Exact official task lifecycle support installed by the trusted caller.
    pub task_lifecycle_capabilities: McpTaskLifecycleCapabilities,
    /// Local lower bound for server-suggested task polling intervals.
    pub task_poll_floor: Duration,
    /// Exact list/resource notification handlers installed by the trusted caller.
    pub subscription_capabilities: McpSubscriptionCapabilities,
    /// Bounded SDK channel capacity for one current-protocol subscription stream.
    pub subscription_channel_capacity: usize,
}

impl McpClientConfig {
    pub fn new(transport: McpTransportConfig) -> Self {
        Self {
            transport,
            limits: McpClientLimits::default(),
            connect_timeout: Duration::from_secs(20),
            request_timeout: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(5),
            continuation_timeout: Duration::from_secs(24 * 60 * 60),
            mrtr_presentation_capabilities: McpMrtrPresentationCapabilities::default(),
            task_lifecycle_capabilities: McpTaskLifecycleCapabilities::default(),
            task_poll_floor: Duration::from_millis(250),
            subscription_capabilities: McpSubscriptionCapabilities::default(),
            subscription_channel_capacity: 64,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), McpClientError> {
        if self.connect_timeout.is_zero()
            || self.request_timeout.is_zero()
            || self.shutdown_timeout.is_zero()
            || self.continuation_timeout.is_zero()
            || self.task_poll_floor.is_zero()
        {
            return Err(McpClientError::InvalidConfig(
                "timeouts must be greater than zero".to_owned(),
            ));
        }
        validate_upper_bound(
            "connect timeout",
            self.connect_timeout,
            HARD_MAX_CONNECT_TIMEOUT,
        )?;
        validate_upper_bound(
            "request timeout",
            self.request_timeout,
            HARD_MAX_REQUEST_TIMEOUT,
        )?;
        validate_upper_bound(
            "shutdown timeout",
            self.shutdown_timeout,
            HARD_MAX_SHUTDOWN_TIMEOUT,
        )?;
        validate_upper_bound(
            "continuation timeout",
            self.continuation_timeout,
            HARD_MAX_CONTINUATION_TIMEOUT,
        )?;
        validate_upper_bound(
            "task poll floor",
            self.task_poll_floor,
            HARD_MAX_TASK_POLL_FLOOR,
        )?;

        let limits = &self.limits;
        if limits.max_tool_count == 0
            || limits.max_tool_pages == 0
            || limits.max_cursor_bytes == 0
            || limits.max_tool_name_bytes == 0
            || limits.max_tool_title_bytes == 0
            || limits.max_tool_description_bytes == 0
            || limits.max_schema_bytes == 0
            || limits.max_catalog_bytes == 0
            || limits.max_request_bytes == 0
            || limits.max_result_bytes == 0
            || limits.max_json_depth == 0
            || limits.max_json_nodes == 0
            || limits.max_sse_event_bytes == 0
            || limits.max_transport_message_bytes == 0
            || limits.max_pending_continuations == 0
            || limits.max_continuation_bytes == 0
            || limits.max_mrtr_inputs == 0
            || limits.max_mrtr_state_only_rounds == 0
            || limits.max_mrtr_interactive_rounds == 0
            || limits.max_task_operations == 0
            || limits.max_task_id_bytes == 0
            || limits.max_task_status_message_bytes == 0
            || limits.max_resource_subscriptions == 0
            || limits.max_resource_uri_bytes == 0
            || limits.max_resource_subscription_bytes == 0
            || self.subscription_channel_capacity == 0
        {
            return Err(McpClientError::InvalidConfig(
                "all resource limits must be greater than zero".to_owned(),
            ));
        }

        for (label, value, maximum) in [
            ("max_tool_count", limits.max_tool_count, HARD_MAX_TOOL_COUNT),
            ("max_tool_pages", limits.max_tool_pages, HARD_MAX_TOOL_PAGES),
            (
                "max_cursor_bytes",
                limits.max_cursor_bytes,
                HARD_MAX_CURSOR_BYTES,
            ),
            (
                "max_tool_name_bytes",
                limits.max_tool_name_bytes,
                HARD_MAX_TOOL_NAME_BYTES,
            ),
            (
                "max_tool_title_bytes",
                limits.max_tool_title_bytes,
                HARD_MAX_TOOL_TITLE_BYTES,
            ),
            (
                "max_tool_description_bytes",
                limits.max_tool_description_bytes,
                HARD_MAX_TOOL_DESCRIPTION_BYTES,
            ),
            (
                "max_schema_bytes",
                limits.max_schema_bytes,
                HARD_MAX_SCHEMA_BYTES,
            ),
            (
                "max_catalog_bytes",
                limits.max_catalog_bytes,
                HARD_MAX_CATALOG_BYTES,
            ),
            (
                "max_request_bytes",
                limits.max_request_bytes,
                HARD_MAX_REQUEST_BYTES,
            ),
            (
                "max_result_bytes",
                limits.max_result_bytes,
                HARD_MAX_RESULT_BYTES,
            ),
            ("max_json_depth", limits.max_json_depth, HARD_MAX_JSON_DEPTH),
            ("max_json_nodes", limits.max_json_nodes, HARD_MAX_JSON_NODES),
            (
                "max_sse_event_bytes",
                limits.max_sse_event_bytes,
                HARD_MAX_SSE_EVENT_BYTES,
            ),
            (
                "max_transport_message_bytes",
                limits.max_transport_message_bytes,
                HARD_MAX_TRANSPORT_MESSAGE_BYTES,
            ),
            (
                "max_pending_continuations",
                limits.max_pending_continuations,
                HARD_MAX_PENDING_CONTINUATIONS,
            ),
            (
                "max_continuation_bytes",
                limits.max_continuation_bytes,
                HARD_MAX_CONTINUATION_BYTES,
            ),
            (
                "max_mrtr_inputs",
                limits.max_mrtr_inputs,
                HARD_MAX_MRTR_INPUTS,
            ),
            (
                "max_mrtr_state_only_rounds",
                limits.max_mrtr_state_only_rounds,
                HARD_MAX_MRTR_STATE_ONLY_ROUNDS,
            ),
            (
                "max_mrtr_interactive_rounds",
                limits.max_mrtr_interactive_rounds,
                HARD_MAX_MRTR_INTERACTIVE_ROUNDS,
            ),
            (
                "max_task_operations",
                limits.max_task_operations,
                HARD_MAX_TASK_OPERATIONS,
            ),
            (
                "max_task_id_bytes",
                limits.max_task_id_bytes,
                HARD_MAX_TASK_ID_BYTES,
            ),
            (
                "max_task_status_message_bytes",
                limits.max_task_status_message_bytes,
                HARD_MAX_TASK_STATUS_MESSAGE_BYTES,
            ),
            (
                "max_resource_subscriptions",
                limits.max_resource_subscriptions,
                HARD_MAX_RESOURCE_SUBSCRIPTIONS,
            ),
            (
                "max_resource_uri_bytes",
                limits.max_resource_uri_bytes,
                HARD_MAX_RESOURCE_URI_BYTES,
            ),
            (
                "max_resource_subscription_bytes",
                limits.max_resource_subscription_bytes,
                HARD_MAX_RESOURCE_SUBSCRIPTION_BYTES,
            ),
            (
                "subscription_channel_capacity",
                self.subscription_channel_capacity,
                HARD_MAX_SUBSCRIPTION_CHANNEL_CAPACITY,
            ),
        ] {
            if value > maximum {
                return Err(McpClientError::InvalidConfig(format!(
                    "{label} exceeds the hard ceiling of {maximum}"
                )));
            }
        }
        let request_with_envelope = limits
            .max_request_bytes
            .saturating_add(limits.max_tool_name_bytes)
            .saturating_add(4 * 1024);
        let result_with_envelope = limits.max_result_bytes.saturating_add(4 * 1024);
        if request_with_envelope > limits.max_transport_message_bytes
            || result_with_envelope > limits.max_transport_message_bytes
            || limits.max_sse_event_bytes > limits.max_transport_message_bytes
        {
            return Err(McpClientError::InvalidConfig(
                "raw transport message limit must cover request/result envelopes and the SSE event limit"
                    .to_owned(),
            ));
        }
        let one_continuation_reservation = limits
            .max_request_bytes
            .checked_add(limits.max_tool_name_bytes)
            .and_then(|bytes| bytes.checked_add(limits.max_result_bytes))
            .ok_or_else(|| {
                McpClientError::InvalidConfig(
                    "continuation reservation limits overflowed".to_owned(),
                )
            })?;
        if one_continuation_reservation > limits.max_continuation_bytes {
            return Err(McpClientError::InvalidConfig(
                "continuation byte limit must retain at least one maximum-size request and pending result"
                    .to_owned(),
            ));
        }

        match &self.transport {
            McpTransportConfig::Stdio(config) => validate_stdio(config),
            McpTransportConfig::StreamableHttp(config) => validate_http(config),
            // Channel construction already enforces a non-zero bounded
            // capacity. Closed peers fail during MCP discovery, where they can
            // be classified as connection failures rather than config errors.
            McpTransportConfig::DuplexJson(_) => Ok(()),
        }
    }
}

fn validate_upper_bound(
    label: &str,
    value: Duration,
    maximum: Duration,
) -> Result<(), McpClientError> {
    if value > maximum {
        return Err(McpClientError::InvalidConfig(format!(
            "{label} exceeds the hard ceiling of {} seconds",
            maximum.as_secs()
        )));
    }
    Ok(())
}

fn validate_stdio(config: &StdioTransportConfig) -> Result<(), McpClientError> {
    if !config.executable.is_absolute() {
        return Err(McpClientError::InvalidConfig(
            "stdio executable must be an already-resolved absolute path".to_owned(),
        ));
    }
    if config.args.len() > 1_024 {
        return Err(McpClientError::InvalidConfig(
            "stdio argument count exceeds 1024".to_owned(),
        ));
    }
    if config.environment.len() > 256 {
        return Err(McpClientError::InvalidConfig(
            "stdio environment binding count exceeds 256".to_owned(),
        ));
    }
    if config.environment.keys().any(|key| key.is_empty()) {
        return Err(McpClientError::InvalidConfig(
            "stdio environment names cannot be empty".to_owned(),
        ));
    }
    if config
        .working_directory
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(McpClientError::InvalidConfig(
            "stdio working directory must be an already-resolved absolute path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_http(config: &StreamableHttpTransportConfig) -> Result<(), McpClientError> {
    let endpoint = url::Url::parse(&config.endpoint).map_err(|_| {
        McpClientError::InvalidConfig("Streamable HTTP endpoint is not a valid URL".to_owned())
    })?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(McpClientError::InvalidConfig(
            "Streamable HTTP endpoint cannot contain embedded credentials".to_owned(),
        ));
    }
    if endpoint.fragment().is_some() {
        return Err(McpClientError::InvalidConfig(
            "Streamable HTTP endpoint cannot contain a fragment".to_owned(),
        ));
    }
    if endpoint.query().is_some() {
        return Err(McpClientError::InvalidConfig(
            "Streamable HTTP endpoint cannot contain a query; credentials belong in Auth Broker bindings"
                .to_owned(),
        ));
    }
    let secure = endpoint.scheme() == "https";
    let loopback_http = endpoint.scheme() == "http"
        && config.allow_loopback_http
        && endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if !secure && !loopback_http {
        return Err(McpClientError::InvalidConfig(
            "Streamable HTTP endpoint must use HTTPS (HTTP is limited to loopback)".to_owned(),
        ));
    }
    if config.bearer_token.as_ref().is_some_and(|token| {
        token.expose().is_empty() || token.expose().chars().any(char::is_control)
    }) {
        return Err(McpClientError::InvalidConfig(
            "bearer token is empty or contains control characters".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrtr_presentation_capabilities_are_explicit_exact_and_non_serializable() {
        static_assertions::assert_not_impl_any!(
            McpMrtrPresentationCapabilities: serde::Serialize, serde::de::DeserializeOwned
        );

        let empty = McpMrtrPresentationCapabilities::default().sdk_client_capabilities();
        assert!(empty.sampling.is_none());
        assert!(empty.elicitation.is_none());
        assert!(empty.roots.is_none());
        assert!(empty.extensions.is_none());

        let configured = McpMrtrPresentationCapabilities::new()
            .enable_sampling()
            .enable_form_elicitation()
            .enable_roots();
        assert!(configured.sampling());
        assert!(configured.form_elicitation());
        assert!(configured.roots());
        let advertised = configured.sdk_client_capabilities();
        let sampling = advertised.sampling.as_ref().unwrap();
        assert!(sampling.tools.is_none());
        assert!(sampling.context.is_none());
        let elicitation = advertised.elicitation.as_ref().unwrap();
        assert_eq!(
            elicitation
                .form
                .as_ref()
                .and_then(|form| form.schema_validation),
            Some(true)
        );
        assert!(elicitation.url.is_none());
        assert_eq!(
            advertised
                .roots
                .as_ref()
                .and_then(|roots| roots.list_changed),
            None
        );
        assert!(advertised.extensions.is_none());
    }

    #[test]
    fn task_lifecycle_capability_is_explicit_exact_and_non_serializable() {
        static_assertions::assert_not_impl_any!(
            McpTaskLifecycleCapabilities: serde::Serialize, serde::de::DeserializeOwned
        );
        let mut empty = rmcp::model::ClientCapabilities::default();
        McpTaskLifecycleCapabilities::default().apply_to(&mut empty);
        assert!(!empty.supports_tasks());
        assert!(empty.extensions.is_none());

        let configured = McpTaskLifecycleCapabilities::new().enable();
        assert!(configured.enabled());
        configured.apply_to(&mut empty);
        assert!(empty.supports_tasks());
        assert_eq!(
            empty.extensions.as_ref().unwrap().len(),
            1,
            "task support must advertise only the exact official extension"
        );
    }

    #[test]
    fn subscription_capabilities_are_default_off_and_intersect_server_support() {
        static_assertions::assert_not_impl_any!(
            McpSubscriptionCapabilities: serde::Serialize, serde::de::DeserializeOwned
        );
        assert!(McpSubscriptionCapabilities::default().is_empty());
        let installed = McpSubscriptionCapabilities::new()
            .enable_tools_list_changed()
            .enable_prompts_list_changed()
            .enable_resources_list_changed()
            .enable_resource_updates();
        let server = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .build();
        let negotiated = installed.supported_by(&server);
        assert!(negotiated.tools_list_changed());
        assert!(!negotiated.prompts_list_changed());
        assert!(!negotiated.resources_list_changed());
        assert!(negotiated.resource_updates());
    }

    #[test]
    fn bearer_token_debug_is_redacted() {
        let token = BearerToken::new("canary-secret");
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("canary-secret"));
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn http_debug_removes_query_credentials() {
        let config = StreamableHttpTransportConfig::new(
            "https://canary-user:canary-password@example.test/mcp?access_token=canary-secret",
        )
        .with_bearer_token(BearerToken::new("another-secret"));
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("canary-secret"));
        assert!(!rendered.contains("canary-user"));
        assert!(!rendered.contains("canary-password"));
        assert!(!rendered.contains("another-secret"));
        assert!(rendered.contains("https://example.test/"));
        assert!(!rendered.contains("/mcp"));
    }

    #[test]
    fn only_https_or_loopback_http_is_accepted() {
        let remote = McpClientConfig::new(McpTransportConfig::StreamableHttp(
            StreamableHttpTransportConfig::new("http://example.test/mcp"),
        ));
        assert!(remote.validate().is_err());

        let loopback = McpClientConfig::new(McpTransportConfig::StreamableHttp(
            StreamableHttpTransportConfig::new("http://127.0.0.1:3000/mcp"),
        ));
        assert!(loopback.validate().is_ok());
    }

    #[test]
    fn bounded_duplex_transport_is_a_valid_governed_transport() {
        let (_to_client, inbound) = mpsc::channel(1);
        let (outbound, _from_client) = mpsc::channel(1);
        let config = McpClientConfig::new(McpTransportConfig::DuplexJson(
            DuplexJsonTransportConfig::new(inbound, outbound),
        ));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn endpoint_queries_are_rejected_as_credential_unsafe() {
        let config = McpClientConfig::new(McpTransportConfig::StreamableHttp(
            StreamableHttpTransportConfig::new("https://example.test/mcp?token=secret"),
        ));
        assert!(config.validate().is_err());
    }

    #[test]
    fn stdio_requires_pre_resolved_absolute_paths() {
        let config =
            McpClientConfig::new(McpTransportConfig::Stdio(StdioTransportConfig::new("node")));
        assert!(config.validate().is_err());
    }

    #[test]
    fn configurable_limits_cannot_disable_hard_resource_ceilings() {
        let mut config = McpClientConfig::new(McpTransportConfig::Stdio(
            StdioTransportConfig::new("/usr/bin/true"),
        ));
        config.limits.max_json_depth = HARD_MAX_JSON_DEPTH + 1;
        assert!(config.validate().is_err());

        config.limits.max_json_depth = HARD_MAX_JSON_DEPTH;
        config.request_timeout = HARD_MAX_REQUEST_TIMEOUT + Duration::from_secs(1);
        assert!(config.validate().is_err());

        config.request_timeout = HARD_MAX_REQUEST_TIMEOUT;
        config.limits.max_mrtr_inputs = HARD_MAX_MRTR_INPUTS + 1;
        assert!(config.validate().is_err());

        config.limits.max_mrtr_inputs = 0;
        assert!(config.validate().is_err());

        config.limits.max_mrtr_inputs = HARD_MAX_MRTR_INPUTS;
        config.limits.max_mrtr_state_only_rounds = HARD_MAX_MRTR_STATE_ONLY_ROUNDS + 1;
        assert!(config.validate().is_err());

        config.limits.max_mrtr_state_only_rounds = HARD_MAX_MRTR_STATE_ONLY_ROUNDS;
        config.limits.max_mrtr_interactive_rounds = 0;
        assert!(config.validate().is_err());

        config.limits.max_mrtr_interactive_rounds = HARD_MAX_MRTR_INTERACTIVE_ROUNDS;
        config.limits.max_resource_subscriptions = HARD_MAX_RESOURCE_SUBSCRIPTIONS + 1;
        assert!(config.validate().is_err());

        config.limits.max_resource_subscriptions = HARD_MAX_RESOURCE_SUBSCRIPTIONS;
        config.subscription_channel_capacity = HARD_MAX_SUBSCRIPTION_CHANNEL_CAPACITY + 1;
        assert!(config.validate().is_err());

        config.subscription_channel_capacity = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn raw_transport_limit_must_cover_projected_payloads_and_sse_events() {
        let mut config = McpClientConfig::new(McpTransportConfig::Stdio(
            StdioTransportConfig::new("/usr/bin/true"),
        ));
        config.limits.max_transport_message_bytes = config.limits.max_result_bytes;
        assert!(config.validate().is_err());

        config.limits.max_transport_message_bytes = 20 * 1024 * 1024;
        config.limits.max_sse_event_bytes = config.limits.max_transport_message_bytes + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn continuation_retention_is_finite_and_can_reserve_one_maximum_payload() {
        let mut config = McpClientConfig::new(McpTransportConfig::Stdio(
            StdioTransportConfig::new("/usr/bin/true"),
        ));
        config.continuation_timeout = Duration::ZERO;
        assert!(config.validate().is_err());

        config.continuation_timeout = HARD_MAX_CONTINUATION_TIMEOUT + Duration::from_secs(1);
        assert!(config.validate().is_err());

        config.continuation_timeout = Duration::from_secs(60);
        config.limits.max_pending_continuations = 0;
        assert!(config.validate().is_err());

        config.limits.max_pending_continuations = 1;
        config.limits.max_continuation_bytes = config
            .limits
            .max_request_bytes
            .saturating_add(config.limits.max_tool_name_bytes)
            .saturating_add(config.limits.max_result_bytes)
            .saturating_sub(1);
        assert!(config.validate().is_err());

        config.limits.max_continuation_bytes = config
            .limits
            .max_request_bytes
            .saturating_add(config.limits.max_tool_name_bytes)
            .saturating_add(config.limits.max_result_bytes);
        assert!(config.validate().is_ok());
    }
}
