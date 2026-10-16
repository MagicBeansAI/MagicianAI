//! The first-party Town Square store, now a source rather than an authority.
//!
//! Queue item 6 moved the corpus into the `town-square` app package. What
//! remains here is the durable SQLite store and its typed rows, kept for two
//! reasons: the one-shot migration reads it, and it stays intact afterwards so
//! a bad migration is recoverable rather than terminal.
//!
//! The autonomous half is gone. The worker that gated, composed and posted on
//! agents' behalf is replaced by the package's `ambient_turn` behavior, and the
//! HTTP surface moved to `magician_api::social_api`, which serves the same
//! wire from the package's entity store.
pub mod store;
pub mod types;
