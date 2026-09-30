//! Product-safe projections for the governed MCP Tasks extension lifecycle.
//!
//! Provider task identifiers, status messages, timestamps, and input-request keys remain
//! private. Public values carry only exact local continuation authority and stable state.

use std::{collections::HashMap, fmt, sync::Mutex, time::Duration};

use rmcp::model::InputRequests;

use crate::{
    mrtr::{prepare_input_responses_from_requests, McpMrtrResponse, McpPreparedMrtrResponses},
    McpClientError, McpClientLimits, McpPendingCall, McpToolCallResult,
};

pub const MCP_TASK_LIFECYCLE_CONTRACT_V1: &str = "magician.mcp-task-lifecycle.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTaskState {
    Working,
    InputRequired,
    CancellationRequested,
}

/// Payload-free current state of one exact retained task revision.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct McpTaskProgress {
    pending: McpPendingCall,
    state: McpTaskState,
    poll_after: Duration,
}

impl McpTaskProgress {
    pub(crate) fn new(pending: McpPendingCall, state: McpTaskState, poll_after: Duration) -> Self {
        Self {
            pending,
            state,
            poll_after,
        }
    }

    pub fn pending(self) -> McpPendingCall {
        self.pending
    }

    pub fn state(self) -> McpTaskState {
        self.state
    }

    /// Time until the next governed poll. A task notification may reduce this to zero.
    pub fn poll_after(self) -> Duration {
        self.poll_after
    }

    pub fn remaining(self) -> Duration {
        self.pending.remaining()
    }
}

impl fmt::Debug for McpTaskProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpTaskProgress")
            .field("contract", &MCP_TASK_LIFECYCLE_CONTRACT_V1)
            .field("pending", &self.pending)
            .field("state", &self.state)
            .field("poll_after", &self.poll_after)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum McpTaskPollOutcome {
    Pending(McpTaskProgress),
    Complete(McpToolCallResult),
    Failed,
    Cancelled,
}

/// Move-only, SDK-private response set for one task `input_required` revision.
pub struct McpPreparedTaskResponses {
    inner: McpPreparedMrtrResponses,
}

impl McpPreparedTaskResponses {
    pub(crate) fn prepare(
        pending: McpPendingCall,
        requests: &InputRequests,
        responses: Vec<McpMrtrResponse>,
        limits: &McpClientLimits,
    ) -> Result<Self, McpClientError> {
        Ok(Self {
            inner: prepare_input_responses_from_requests(
                pending,
                Some(requests),
                responses,
                limits,
            )?,
        })
    }

    pub fn pending(&self) -> McpPendingCall {
        self.inner.pending()
    }

    pub fn response_count(&self) -> usize {
        self.inner.response_count()
    }

    pub(crate) fn accounted_bytes(&self) -> usize {
        self.inner.accounted_bytes()
    }

    pub(crate) fn into_responses(self) -> Option<rmcp::model::InputResponses> {
        self.inner.into_responses()
    }
}

impl fmt::Debug for McpPreparedTaskResponses {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpPreparedTaskResponses")
            .field("contract", &MCP_TASK_LIFECYCLE_CONTRACT_V1)
            .field("pending", &self.inner.pending())
            .field("response_count", &self.inner.response_count())
            .field("accounted_bytes", &self.inner.accounted_bytes())
            .finish()
    }
}

/// Bounded notification coalescer. Notifications never carry lifecycle authority: this
/// structure records only whether an already-retained private task id has a pending hint.
pub(crate) struct McpTaskNotificationHints {
    max_entries: usize,
    state: Mutex<HashMap<String, bool>>,
}

impl McpTaskNotificationHints {
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn register(&self, task_id: &str) -> Result<(), McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        if state.contains_key(task_id) {
            return Err(McpClientError::ResponseRejected(
                "server reused an active task identifier".to_owned(),
            ));
        }
        if state.len() >= self.max_entries {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        state.insert(task_id.to_owned(), false);
        Ok(())
    }

    pub(crate) fn unregister(&self, task_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.remove(task_id);
        }
    }

    pub(crate) fn mark(&self, task_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(hinted) = state.get_mut(task_id) {
                *hinted = true;
            }
        }
    }

    pub(crate) fn take(&self, task_id: &str) -> Result<bool, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let Some(hinted) = state.get_mut(task_id) else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        Ok(std::mem::take(hinted))
    }

    pub(crate) fn contains_hint(&self, task_id: &str) -> Result<bool, McpClientError> {
        let state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        state
            .get(task_id)
            .copied()
            .ok_or(McpClientError::ContinuationStateUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use static_assertions::assert_not_impl_any;

    use super::*;

    assert_not_impl_any!(
        McpPreparedTaskResponses: Clone,
        serde::Serialize,
        serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        McpTaskProgress: serde::Serialize,
        serde::de::DeserializeOwned
    );

    #[test]
    fn task_hints_are_bounded_registered_only_and_coalesced() {
        let hints = McpTaskNotificationHints::new(1);
        hints.mark("unknown");
        assert!(hints.register("private-one").is_ok());
        assert!(matches!(
            hints.register("private-two"),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        hints.mark("private-one");
        hints.mark("private-one");
        assert!(hints.take("private-one").unwrap());
        assert!(!hints.take("private-one").unwrap());
        hints.unregister("private-one");
        assert!(hints.register("private-two").is_ok());
    }
}
