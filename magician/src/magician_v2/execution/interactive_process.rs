//! Facade re-export for the PTY session subsystem.
//!
//! The implementation lives in the `magician-pty` workspace crate so
//! `portable-pty` doesn't pull through magician on every source edit.
//! Re-exported here under the historical path so internal callers
//! continue to import `crate::magician_v2::execution::interactive_process::*`
//! without any source change.

pub use magician_pty::session::*;
pub use magician_pty::{PtyChunkEvent, PtyEventSink};
