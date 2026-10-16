use std::sync::Arc;

use crate::magician_v2::{
    ask_loop::AskLoopApi, realtime_events::RuntimeTransportBroadcaster,
    transport_log::WorkspaceEventLogRegistry, MagicianV2Orchestrator,
};

/// Fully initialized Magician V2 components bundled together for reuse
pub struct MagicianService {
    pub orchestrator: Arc<MagicianV2Orchestrator>,
    pub event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    pub workspace_event_log_registry: WorkspaceEventLogRegistry,
    pub ask_loop_api: Arc<AskLoopApi>,
}

impl MagicianService {
    pub fn new(
        orchestrator: Arc<MagicianV2Orchestrator>,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
        workspace_event_log_registry: WorkspaceEventLogRegistry,
        ask_loop_api: Arc<AskLoopApi>,
    ) -> Self {
        Self {
            orchestrator,
            event_broadcaster,
            workspace_event_log_registry,
            ask_loop_api,
        }
    }
}
