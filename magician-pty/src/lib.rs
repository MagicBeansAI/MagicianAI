//! PTY-backed interactive session registry, extracted from magician so
//! the `portable-pty` direct dep + ~10 transitive crates don't recompile
//! on every magician source edit.
//!
//! The session core lives in `session.rs`. The trait `PtyEventSink`
//! abstracts the realtime event bus so this crate stays decoupled
//! from magician's `RuntimeTransportBroadcaster` — magician implements
//! `PtyEventSink` for its broadcaster type.

pub mod session;

pub use session::*;

/// One PTY chunk to fan out to the realtime event bus. Bytes are
/// already base64-encoded so SSE/JSON callers can carry binary.
#[derive(Clone, Debug)]
pub struct PtyChunkEvent {
    pub session_id: String,
    pub principal: String,
    pub workspace: String,
    pub ui_thread_id: Option<String>,
    pub program: Option<String>,
    pub offset_start: u64,
    pub offset_end: u64,
    pub bytes_b64: String,
    pub timestamp_ms: i64,
}

/// Cross-crate hook for the realtime event bus. Magician implements
/// this for `RuntimeTransportBroadcaster` (in `realtime_events.rs`).
pub trait PtyEventSink: Send + Sync {
    fn emit_pty_chunk(&self, event: PtyChunkEvent);
}
