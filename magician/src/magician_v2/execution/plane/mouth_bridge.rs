//! The mouth bridge: how a swapped chat mouth reaches the tools that only
//! the native chat dispatcher can run (chat-runtime tools, structural
//! task-spawning tools, personality/skill switches). The chat service
//! builds one per harness turn at its call site, closing over the turn's
//! exact dispatch context; the plane's `tools/call` invokes it for every
//! `PlanePosture::Bridged` name (Tasks 10–11 of
//! `docs/archive/plans/2026-09-13-chat-harness-mouth-parity.md` wire both ends).

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Dispatches one tool call through the native mouth's own dispatcher with
/// the turn's context: `(call_id, name, arguments, cancel)`.
///
/// `call_id` is the plane's invocation ref for the call — the id its chat
/// tool events and turn-ledger record carry — and the bridge runs the
/// native dispatch under that same id, so the native dispatcher's own
/// events and subscriptions for the call never name a second one.
///
/// `cancel` is a child of the grant's dispatch token, cancelled when the
/// grant is revoked while the call is in flight (the turn settling, a
/// harness timeout); the bridge hands it to the native dispatcher as the
/// call's cancel token so the native work stops with the door.
///
/// Returns the dispatcher's JSON result verbatim; the plane runs the
/// provider-boundary sanitizer on it before anything is derived from it.
pub type ChatMouthBridge = Arc<
    dyn Fn(String, String, Value, CancellationToken) -> BoxFuture<'static, Value> + Send + Sync,
>;
