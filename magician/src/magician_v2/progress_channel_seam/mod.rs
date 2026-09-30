//! The progress-channel seam: channel trait, types, router, surface routing,
//! and normalization — the vocabulary chat consumes. The durable plumbing
//! (event log, lineage, storage, channel implementations) lives in the
//! magician-surfaces crate.

pub mod agent_memory;
pub mod channel;
pub mod channels;
pub mod chat_channel;
pub mod event_log;
pub mod lineage;
pub mod normalize;
pub mod router;
pub mod storage;
pub mod surface_routing;
pub mod types;
pub mod webhook;

pub use agent_memory::AgentMemoryChannel;
pub use channel::ProgressChannel;
pub use chat_channel::ChatChannel;
pub use router::ExecutionProgressRouter;
pub use surface_routing::chat_surface_renders_agent_event;
pub use types::{
    ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource, Subscription,
    SubscriptionFilter, SubscriptionSource,
};
pub use webhook::WebhookChannel;
