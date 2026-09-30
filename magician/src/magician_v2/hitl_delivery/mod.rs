//! Critical-request delivery through registered channels (secure HITL plan
//! §6.1, P5).
//!
//! One coordinator owns the `hitl.requested` / `hitl.resolved` subscription
//! and projects a *critical* request — a credential or code ask, a time-bound
//! decision — onto the owner's verified private destinations: registered
//! mobile push, and the channel bots (Kapso WhatsApp, Telegram) the owner
//! enabled. What goes out is a value-free card with a link to the exact
//! request; what is kept is routing metadata only. Attention remains the
//! authoritative pending request whatever the providers do.
//!
//! - [`alert`] — criticality and the safe card.
//! - [`policy`] — destinations, fan-out policy, quiet hours.
//! - [`records`] — the bounded durable delivery log and its latency summary.
//! - [`coordinator`] — the subscription, the sends, retries, claims and
//!   reports, retirement on resolution.
use std::sync::{Arc, OnceLock};

pub mod alert;
pub mod coordinator;
pub mod oracle;
pub mod policy;
pub mod records;

pub use alert::{AlertCard, Criticality};
pub use coordinator::{
    ChannelTransport, ClaimError, ClaimGrant, DeliveryCoordinator, DeliveryOutcome, PushSink,
    PushWave, ReportError, RequestIdentity, RequestOracle, RetrievalOracle, StatusReport,
};
pub use oracle::RuntimeRequestOracle;
pub use policy::DeliveryPolicy;
pub use records::{
    ClaimBinding, DeliveryRecord, DeliveryState, DeliveryStore, Destination, LatencySummary,
};

static GLOBAL: OnceLock<Arc<DeliveryCoordinator>> = OnceLock::new();

/// Publish the process's coordinator so the API layer (claims, reports,
/// status, settings reload, the test action) reaches it the way the device
/// governance stores are reached.
pub fn install_global(coordinator: Arc<DeliveryCoordinator>) {
    let _ = GLOBAL.set(coordinator);
}

pub fn global() -> Option<Arc<DeliveryCoordinator>> {
    GLOBAL.get().cloned()
}
