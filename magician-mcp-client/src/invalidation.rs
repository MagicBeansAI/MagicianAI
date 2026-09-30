//! Governed, bounded ownership for MCP notification subscriptions and invalidations.
//!
//! Remote resource URIs and SDK subscription handles remain private. Notifications only
//! advance fixed-size local epochs; they never mutate a catalog or become discovery
//! authority. A successful authoritative refresh acknowledges only the epoch observed
//! before it began, so a concurrent notification remains pending.

use std::{
    collections::BTreeSet,
    fmt,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use rmcp::{
    model::{
        ProtocolVersion, ServerNotification, SubscribeRequestParams, SubscriptionFilter,
        UnsubscribeRequestParams,
    },
    service::{Peer, RoleClient, Subscription},
};
use tokio::sync::Notify;

use crate::{McpClientError, McpClientLimits, McpSubscriptionCapabilities};

pub const MCP_INVALIDATION_CONTRACT_V1: &str = "magician.mcp-invalidation.v1";

/// Move-only exact subscription request authored by a trusted caller.
pub struct McpSubscriptionRequest {
    capabilities: McpSubscriptionCapabilities,
    resource_uris: Vec<String>,
}

impl McpSubscriptionRequest {
    pub fn new() -> Self {
        Self {
            capabilities: McpSubscriptionCapabilities::new(),
            resource_uris: Vec::new(),
        }
    }

    pub fn with_tools_list_changed(mut self) -> Self {
        self.capabilities = self.capabilities.enable_tools_list_changed();
        self
    }

    pub fn with_prompts_list_changed(mut self) -> Self {
        self.capabilities = self.capabilities.enable_prompts_list_changed();
        self
    }

    pub fn with_resources_list_changed(mut self) -> Self {
        self.capabilities = self.capabilities.enable_resources_list_changed();
        self
    }

    pub fn with_resource_uri(mut self, uri: impl Into<String>) -> Self {
        self.capabilities = self.capabilities.enable_resource_updates();
        self.resource_uris.push(uri.into());
        self
    }

    pub fn capabilities(&self) -> McpSubscriptionCapabilities {
        self.capabilities
    }

    pub fn resource_count(&self) -> usize {
        self.resource_uris.len()
    }
}

impl Default for McpSubscriptionRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for McpSubscriptionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpSubscriptionRequest")
            .field("contract", &MCP_INVALIDATION_CONTRACT_V1)
            .field("capabilities", &self.capabilities)
            .field("resource_count", &self.resource_uris.len())
            .finish()
    }
}

/// Payload-free invalidation state. Resource URIs and notification bodies are absent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct McpInvalidationState {
    subscription_active: bool,
    tools_changed: bool,
    prompts_changed: bool,
    resources_changed: bool,
    resource_updates: bool,
}

impl McpInvalidationState {
    pub fn subscription_active(self) -> bool {
        self.subscription_active
    }

    pub fn tools_changed(self) -> bool {
        self.tools_changed
    }

    pub fn prompts_changed(self) -> bool {
        self.prompts_changed
    }

    pub fn resources_changed(self) -> bool {
        self.resources_changed
    }

    pub fn resource_updates(self) -> bool {
        self.resource_updates
    }

    pub fn any(self) -> bool {
        self.tools_changed
            || self.prompts_changed
            || self.resources_changed
            || self.resource_updates
    }
}

impl fmt::Debug for McpInvalidationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpInvalidationState")
            .field("contract", &MCP_INVALIDATION_CONTRACT_V1)
            .field("subscription_active", &self.subscription_active)
            .field("tools_changed", &self.tools_changed)
            .field("prompts_changed", &self.prompts_changed)
            .field("resources_changed", &self.resources_changed)
            .field("resource_updates", &self.resource_updates)
            .finish()
    }
}

#[derive(Default)]
struct InvalidationEpoch {
    current: u64,
    acknowledged: u64,
    exhausted: bool,
}

impl InvalidationEpoch {
    fn mark(&mut self) {
        if self.exhausted {
            return;
        }
        match self.current.checked_add(1) {
            Some(next) => self.current = next,
            None => self.exhausted = true,
        }
    }

    fn dirty(&self) -> bool {
        self.exhausted || self.current != self.acknowledged
    }

    fn ticket(&self) -> Option<u64> {
        self.dirty().then_some(self.current)
    }

    fn acknowledge(&mut self, ticket: u64) {
        if !self.exhausted && ticket <= self.current {
            self.acknowledged = self.acknowledged.max(ticket);
        }
    }
}

struct ActiveSubscription {
    id: u64,
    accepting: bool,
    capabilities: McpSubscriptionCapabilities,
    resource_uris: BTreeSet<String>,
}

struct InvalidationOwnerState {
    next_subscription_id: u64,
    active: Option<ActiveSubscription>,
    event_revision: u64,
    event_revision_exhausted: bool,
    tools: InvalidationEpoch,
    prompts: InvalidationEpoch,
    resources: InvalidationEpoch,
    resource_updates: InvalidationEpoch,
}

pub(crate) struct McpInvalidationOwner {
    state: Mutex<InvalidationOwnerState>,
    notify: Notify,
}

impl McpInvalidationOwner {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(InvalidationOwnerState {
                next_subscription_id: 1,
                active: None,
                event_revision: 0,
                event_revision_exhausted: false,
                tools: InvalidationEpoch::default(),
                prompts: InvalidationEpoch::default(),
                resources: InvalidationEpoch::default(),
                resource_updates: InvalidationEpoch::default(),
            }),
            notify: Notify::new(),
        }
    }

    pub(crate) fn begin(
        &self,
        capabilities: McpSubscriptionCapabilities,
        resource_uris: BTreeSet<String>,
    ) -> Result<u64, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        if state.active.is_some() {
            return Err(McpClientError::SubscriptionAlreadyActive);
        }
        let id = state.next_subscription_id;
        state.next_subscription_id = state
            .next_subscription_id
            .checked_add(1)
            .ok_or(McpClientError::SubscriptionIdentityExhausted)?;
        state.active = Some(ActiveSubscription {
            id,
            accepting: false,
            capabilities,
            resource_uris,
        });
        Ok(id)
    }

    pub(crate) fn activate(&self, id: u64) -> Result<u64, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        let Some(active) = state.active.as_mut() else {
            return Err(McpClientError::SubscriptionNotActive);
        };
        if active.id != id || active.accepting {
            return Err(McpClientError::SubscriptionNotActive);
        }
        active.accepting = true;
        mark_all_active(&mut state, id)?;
        let revision = state.event_revision;
        drop(state);
        self.notify.notify_waiters();
        Ok(revision)
    }

    pub(crate) fn release(&self, id: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.active = None;
            bump_event_revision(&mut state);
            drop(state);
            self.notify.notify_waiters();
        }
    }

    pub(crate) fn state(&self) -> Result<McpInvalidationState, McpClientError> {
        let state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        Ok(public_state(&state))
    }

    pub(crate) fn tools_ticket(&self) -> Result<Option<u64>, McpClientError> {
        self.state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)
            .map(|state| state.tools.ticket())
    }

    pub(crate) fn acknowledge_tools(&self, ticket: u64) -> Result<(), McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        state.tools.acknowledge(ticket);
        Ok(())
    }

    #[cfg(test)]
    fn resource_updates_ticket(&self) -> Result<Option<u64>, McpClientError> {
        self.state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)
            .map(|state| state.resource_updates.ticket())
    }

    #[cfg(test)]
    fn acknowledge_resource_updates(&self, ticket: u64) -> Result<(), McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        state.resource_updates.acknowledge(ticket);
        Ok(())
    }

    pub(crate) fn mark_tools(&self) {
        self.mark_kind(InvalidationKind::Tools, None);
    }

    pub(crate) fn mark_prompts(&self) {
        self.mark_kind(InvalidationKind::Prompts, None);
    }

    pub(crate) fn mark_resources(&self) {
        self.mark_kind(InvalidationKind::Resources, None);
    }

    pub(crate) fn mark_resource(&self, uri: &str) {
        self.mark_kind(InvalidationKind::ResourceUpdate, Some(uri));
    }

    fn mark_kind(&self, kind: InvalidationKind, uri: Option<&str>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !active_accepts(&state, kind, uri) {
            return;
        }
        mark_epoch(&mut state, kind);
        bump_event_revision(&mut state);
        drop(state);
        self.notify.notify_waiters();
    }

    pub(crate) fn mark_notification(
        &self,
        id: u64,
        notification: &ServerNotification,
    ) -> Result<(), McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        let (kind, uri) = match notification {
            ServerNotification::ToolListChangedNotification(_) => (InvalidationKind::Tools, None),
            ServerNotification::PromptListChangedNotification(_) => {
                (InvalidationKind::Prompts, None)
            },
            ServerNotification::ResourceListChangedNotification(_) => {
                (InvalidationKind::Resources, None)
            },
            ServerNotification::ResourceUpdatedNotification(update) => (
                InvalidationKind::ResourceUpdate,
                Some(update.params.uri.as_str()),
            ),
            _ => return Err(McpClientError::SubscriptionProtocolViolation),
        };
        let matching_id = state.active.as_ref().is_some_and(|active| active.id == id);
        if !matching_id || !active_accepts(&state, kind, uri) {
            return Err(McpClientError::SubscriptionProtocolViolation);
        }
        mark_epoch(&mut state, kind);
        bump_event_revision(&mut state);
        drop(state);
        self.notify.notify_waiters();
        Ok(())
    }

    pub(crate) fn mark_all(&self, id: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if mark_all_active(&mut state, id).is_ok() {
            drop(state);
            self.notify.notify_waiters();
        }
    }

    async fn wait_for_change(
        &self,
        id: u64,
        observed: u64,
    ) -> Result<(u64, McpInvalidationState), McpClientError> {
        loop {
            let notified = self.notify.notified();
            {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
                let active = state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == id && active.accepting);
                if !active {
                    return Err(McpClientError::SubscriptionNotActive);
                }
                if state.event_revision_exhausted || state.event_revision != observed {
                    return Ok((state.event_revision, public_state(&state)));
                }
            }
            notified.await;
        }
    }
}

#[derive(Clone, Copy)]
enum InvalidationKind {
    Tools,
    Prompts,
    Resources,
    ResourceUpdate,
}

fn active_accepts(
    state: &InvalidationOwnerState,
    kind: InvalidationKind,
    uri: Option<&str>,
) -> bool {
    let Some(active) = state.active.as_ref().filter(|active| active.accepting) else {
        return false;
    };
    match kind {
        InvalidationKind::Tools => active.capabilities.tools_list_changed(),
        InvalidationKind::Prompts => active.capabilities.prompts_list_changed(),
        InvalidationKind::Resources => active.capabilities.resources_list_changed(),
        InvalidationKind::ResourceUpdate => {
            active.capabilities.resource_updates()
                && uri.is_some_and(|uri| active.resource_uris.contains(uri))
        },
    }
}

fn mark_epoch(state: &mut InvalidationOwnerState, kind: InvalidationKind) {
    match kind {
        InvalidationKind::Tools => state.tools.mark(),
        InvalidationKind::Prompts => state.prompts.mark(),
        InvalidationKind::Resources => state.resources.mark(),
        InvalidationKind::ResourceUpdate => state.resource_updates.mark(),
    }
}

fn mark_all_active(state: &mut InvalidationOwnerState, id: u64) -> Result<(), McpClientError> {
    let Some(active) = state
        .active
        .as_ref()
        .filter(|active| active.id == id && active.accepting)
    else {
        return Err(McpClientError::SubscriptionNotActive);
    };
    let capabilities = active.capabilities;
    if capabilities.tools_list_changed() {
        state.tools.mark();
    }
    if capabilities.prompts_list_changed() {
        state.prompts.mark();
    }
    if capabilities.resources_list_changed() {
        state.resources.mark();
    }
    if capabilities.resource_updates() {
        state.resource_updates.mark();
    }
    bump_event_revision(state);
    Ok(())
}

fn bump_event_revision(state: &mut InvalidationOwnerState) {
    if state.event_revision_exhausted {
        return;
    }
    match state.event_revision.checked_add(1) {
        Some(next) => state.event_revision = next,
        None => state.event_revision_exhausted = true,
    }
}

fn public_state(state: &InvalidationOwnerState) -> McpInvalidationState {
    McpInvalidationState {
        subscription_active: state.active.as_ref().is_some_and(|active| active.accepting),
        tools_changed: state.tools.dirty(),
        prompts_changed: state.prompts.dirty(),
        resources_changed: state.resources.dirty(),
        resource_updates: state.resource_updates.dirty(),
    }
}

pub(crate) struct ValidatedSubscriptionRequest {
    pub(crate) capabilities: McpSubscriptionCapabilities,
    pub(crate) resource_uris: BTreeSet<String>,
}

impl ValidatedSubscriptionRequest {
    pub(crate) fn sdk_filter(&self) -> SubscriptionFilter {
        let mut filter = SubscriptionFilter::new();
        filter.tools_list_changed = self.capabilities.tools_list_changed().then_some(true);
        filter.prompts_list_changed = self.capabilities.prompts_list_changed().then_some(true);
        filter.resources_list_changed = self.capabilities.resources_list_changed().then_some(true);
        filter.resource_subscriptions =
            (!self.resource_uris.is_empty()).then(|| self.resource_uris.iter().cloned().collect());
        filter
    }

    pub(crate) fn exactly_acknowledged(&self, accepted: &SubscriptionFilter) -> bool {
        let accepted_uris = accepted
            .resource_subscriptions
            .as_ref()
            .map(|values| values.iter().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        accepted.tools_list_changed == self.capabilities.tools_list_changed().then_some(true)
            && accepted.prompts_list_changed
                == self.capabilities.prompts_list_changed().then_some(true)
            && accepted.resources_list_changed
                == self.capabilities.resources_list_changed().then_some(true)
            && accepted_uris == self.resource_uris
    }
}

pub(crate) fn validate_subscription_request(
    request: McpSubscriptionRequest,
    installed: McpSubscriptionCapabilities,
    negotiated: McpSubscriptionCapabilities,
    limits: &McpClientLimits,
) -> Result<ValidatedSubscriptionRequest, McpClientError> {
    let requested = request.capabilities;
    if requested.is_empty() {
        return Err(McpClientError::SubscriptionUnsupported);
    }
    for (enabled, allowed) in [
        (
            requested.tools_list_changed(),
            installed.tools_list_changed() && negotiated.tools_list_changed(),
        ),
        (
            requested.prompts_list_changed(),
            installed.prompts_list_changed() && negotiated.prompts_list_changed(),
        ),
        (
            requested.resources_list_changed(),
            installed.resources_list_changed() && negotiated.resources_list_changed(),
        ),
        (
            requested.resource_updates(),
            installed.resource_updates() && negotiated.resource_updates(),
        ),
    ] {
        if enabled && !allowed {
            return Err(McpClientError::SubscriptionUnsupported);
        }
    }
    if requested.resource_updates() == request.resource_uris.is_empty()
        || request.resource_uris.len() > limits.max_resource_subscriptions
    {
        return Err(McpClientError::SubscriptionRequestRejected);
    }
    let mut resource_uris = BTreeSet::new();
    let mut total_bytes = 0usize;
    for uri in request.resource_uris {
        if uri.is_empty()
            || uri.len() > limits.max_resource_uri_bytes
            || uri.chars().any(char::is_control)
            || url::Url::parse(&uri).is_err()
        {
            return Err(McpClientError::SubscriptionRequestRejected);
        }
        total_bytes = total_bytes
            .checked_add(uri.len())
            .ok_or(McpClientError::SubscriptionRequestRejected)?;
        if total_bytes > limits.max_resource_subscription_bytes || !resource_uris.insert(uri) {
            return Err(McpClientError::SubscriptionRequestRejected);
        }
    }
    Ok(ValidatedSubscriptionRequest {
        capabilities: requested,
        resource_uris,
    })
}

enum SubscriptionBackend {
    Current(Subscription),
    Legacy {
        peer: Peer<RoleClient>,
        resource_uris: Vec<String>,
    },
}

/// Move-only owner of one exact SDK subscription lifecycle.
#[must_use = "a notification subscription must be consumed or explicitly cancelled"]
pub struct McpNotificationSubscription {
    backend: SubscriptionBackend,
    owner: Arc<McpInvalidationOwner>,
    id: u64,
    accepted: McpSubscriptionCapabilities,
    observed_revision: u64,
    request_timeout: Duration,
    active: bool,
}

impl McpNotificationSubscription {
    pub(crate) fn current(
        subscription: Subscription,
        owner: Arc<McpInvalidationOwner>,
        id: u64,
        accepted: McpSubscriptionCapabilities,
        observed_revision: u64,
        request_timeout: Duration,
    ) -> Self {
        Self {
            backend: SubscriptionBackend::Current(subscription),
            owner,
            id,
            accepted,
            observed_revision,
            request_timeout,
            active: true,
        }
    }

    pub(crate) fn legacy(
        peer: Peer<RoleClient>,
        resource_uris: Vec<String>,
        owner: Arc<McpInvalidationOwner>,
        id: u64,
        accepted: McpSubscriptionCapabilities,
        observed_revision: u64,
        request_timeout: Duration,
    ) -> Self {
        Self {
            backend: SubscriptionBackend::Legacy {
                peer,
                resource_uris,
            },
            owner,
            id,
            accepted,
            observed_revision,
            request_timeout,
            active: true,
        }
    }

    pub fn accepted_capabilities(&self) -> McpSubscriptionCapabilities {
        self.accepted
    }

    pub fn state(&self) -> Result<McpInvalidationState, McpClientError> {
        self.owner.state()
    }

    /// Wait for and coalesce one authoritative SDK notification.
    pub async fn next(&mut self) -> Result<McpInvalidationState, McpClientError> {
        if !self.active {
            return Err(McpClientError::SubscriptionNotActive);
        }
        match &mut self.backend {
            SubscriptionBackend::Current(subscription) => match subscription.next().await {
                Ok(Some(notification)) => {
                    self.owner.mark_notification(self.id, &notification)?;
                    let state = self.owner.state()?;
                    self.observed_revision = self.owner.event_revision(self.id)?;
                    Ok(state)
                },
                Ok(None) | Err(_) => {
                    self.owner.mark_all(self.id);
                    self.owner.release(self.id);
                    self.active = false;
                    Err(McpClientError::SubscriptionEnded)
                },
            },
            SubscriptionBackend::Legacy { .. } => {
                let (revision, state) = self
                    .owner
                    .wait_for_change(self.id, self.observed_revision)
                    .await?;
                self.observed_revision = revision;
                Ok(state)
            },
        }
    }

    /// Cancel the exact SDK subscription. Legacy resource unsubscriptions are all
    /// attempted before the local lease is released.
    #[allow(deprecated)]
    pub async fn cancel(mut self) -> Result<(), McpClientError> {
        if !self.active {
            return Ok(());
        }
        let result = match &mut self.backend {
            SubscriptionBackend::Current(subscription) => {
                tokio::time::timeout(self.request_timeout, subscription.cancel())
                    .await
                    .map_err(|_| McpClientError::SubscriptionTimeout)?
                    .map_err(|_| McpClientError::SubscriptionFailed)
            },
            SubscriptionBackend::Legacy {
                peer,
                resource_uris,
            } => {
                let mut failed = false;
                for uri in resource_uris.drain(..) {
                    let outcome = tokio::time::timeout(
                        self.request_timeout,
                        peer.unsubscribe(UnsubscribeRequestParams::new(uri)),
                    )
                    .await;
                    if !matches!(outcome, Ok(Ok(()))) {
                        failed = true;
                    }
                }
                if failed {
                    Err(McpClientError::SubscriptionFailed)
                } else {
                    Ok(())
                }
            },
        };
        self.owner.release(self.id);
        self.active = false;
        result
    }
}

impl fmt::Debug for McpNotificationSubscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpNotificationSubscription")
            .field("contract", &MCP_INVALIDATION_CONTRACT_V1)
            .field("accepted", &self.accepted)
            .field("active", &self.active)
            .finish()
    }
}

impl Drop for McpNotificationSubscription {
    fn drop(&mut self) {
        if self.active {
            self.owner.release(self.id);
            self.active = false;
        }
    }
}

impl McpInvalidationOwner {
    fn event_revision(&self, id: u64) -> Result<u64, McpClientError> {
        let state = self
            .state
            .lock()
            .map_err(|_| McpClientError::SubscriptionStateUnavailable)?;
        if !state
            .active
            .as_ref()
            .is_some_and(|active| active.id == id && active.accepting)
        {
            return Err(McpClientError::SubscriptionNotActive);
        }
        Ok(state.event_revision)
    }
}

pub(crate) fn protocol_uses_current_subscriptions(version: &ProtocolVersion) -> bool {
    version >= &ProtocolVersion::V_2026_07_28
}

pub(crate) fn subscription_channel_capacity(
    capacity: usize,
) -> Result<NonZeroUsize, McpClientError> {
    NonZeroUsize::new(capacity).ok_or_else(|| {
        McpClientError::InvalidConfig(
            "subscription channel capacity must be greater than zero".to_owned(),
        )
    })
}

#[allow(deprecated)]
pub(crate) async fn subscribe_legacy_resources(
    peer: &Peer<RoleClient>,
    resource_uris: &BTreeSet<String>,
    request_timeout: Duration,
) -> Result<(), McpClientError> {
    let mut subscribed = Vec::new();
    for uri in resource_uris {
        let outcome = tokio::time::timeout(
            request_timeout,
            peer.subscribe(SubscribeRequestParams::new(uri.clone())),
        )
        .await;
        if !matches!(outcome, Ok(Ok(()))) {
            for subscribed_uri in subscribed {
                let _ = tokio::time::timeout(
                    request_timeout,
                    peer.unsubscribe(UnsubscribeRequestParams::new(subscribed_uri)),
                )
                .await;
            }
            return Err(match outcome {
                Err(_) => McpClientError::SubscriptionTimeout,
                Ok(Err(_)) => McpClientError::SubscriptionFailed,
                Ok(Ok(())) => McpClientError::SubscriptionFailed,
            });
        }
        subscribed.push(uri.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use static_assertions::assert_not_impl_any;

    use super::*;

    assert_not_impl_any!(
        McpSubscriptionRequest: Clone,
        serde::Serialize,
        serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        McpNotificationSubscription: Clone,
        serde::Serialize,
        serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        McpInvalidationState: serde::Serialize,
        serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        McpSubscriptionCapabilities: serde::Serialize,
        serde::de::DeserializeOwned
    );

    fn all_capabilities() -> McpSubscriptionCapabilities {
        McpSubscriptionCapabilities::new()
            .enable_tools_list_changed()
            .enable_prompts_list_changed()
            .enable_resources_list_changed()
            .enable_resource_updates()
    }

    #[test]
    fn request_validation_is_exact_bounded_and_value_free() {
        let limits = McpClientLimits {
            max_resource_subscriptions: 1,
            max_resource_uri_bytes: 64,
            max_resource_subscription_bytes: 64,
            ..McpClientLimits::default()
        };
        let request = McpSubscriptionRequest::new()
            .with_tools_list_changed()
            .with_resource_uri("file:///notes/one");
        let debug = format!("{request:?}");
        assert!(debug.contains("resource_count: 1"));
        assert!(!debug.contains("file:///notes/one"));
        let validated =
            validate_subscription_request(request, all_capabilities(), all_capabilities(), &limits)
                .unwrap();
        assert_eq!(validated.resource_uris.len(), 1);

        let duplicate = McpSubscriptionRequest::new()
            .with_resource_uri("file:///notes/private")
            .with_resource_uri("file:///notes/private");
        assert!(matches!(
            validate_subscription_request(
                duplicate,
                all_capabilities(),
                all_capabilities(),
                &limits,
            ),
            Err(McpClientError::SubscriptionRequestRejected)
        ));
    }

    #[test]
    fn revisioned_acknowledgement_cannot_clear_a_concurrent_notification() {
        let owner = McpInvalidationOwner::new();
        let id = owner
            .begin(
                McpSubscriptionCapabilities::new().enable_tools_list_changed(),
                BTreeSet::new(),
            )
            .unwrap();
        owner.activate(id).unwrap();
        let first = owner.tools_ticket().unwrap().unwrap();
        owner.mark_tools();
        owner.acknowledge_tools(first).unwrap();
        assert!(owner.state().unwrap().tools_changed());
        let second = owner.tools_ticket().unwrap().unwrap();
        owner.acknowledge_tools(second).unwrap();
        assert!(!owner.state().unwrap().tools_changed());
    }

    #[test]
    fn unknown_and_unsubscribed_resource_notifications_are_ignored() {
        let owner = McpInvalidationOwner::new();
        owner.mark_tools();
        assert!(!owner.state().unwrap().any());
        let id = owner
            .begin(
                all_capabilities(),
                ["file:///allowed".to_owned()].into_iter().collect(),
            )
            .unwrap();
        owner.activate(id).unwrap();
        let initial = owner.state().unwrap();
        assert!(initial.any());
        let ticket = owner.tools_ticket().unwrap().unwrap();
        owner.acknowledge_tools(ticket).unwrap();
        let resource_ticket = owner.resource_updates_ticket().unwrap().unwrap();
        owner.acknowledge_resource_updates(resource_ticket).unwrap();
        owner.mark_resource("file:///not-allowed");
        assert!(!owner.state().unwrap().resource_updates());
        owner.mark_resource("file:///allowed");
        assert!(owner.state().unwrap().resource_updates());
    }

    #[test]
    fn duplicate_invalidations_are_fixed_memory_and_non_recursive() {
        let join = std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let owner = McpInvalidationOwner::new();
                let id = owner
                    .begin(
                        McpSubscriptionCapabilities::new().enable_tools_list_changed(),
                        BTreeSet::new(),
                    )
                    .unwrap();
                owner.activate(id).unwrap();
                for _ in 0..100_000 {
                    owner.mark_tools();
                }
                assert!(owner.state().unwrap().tools_changed());
            })
            .unwrap();
        join.join().unwrap();
    }
}
