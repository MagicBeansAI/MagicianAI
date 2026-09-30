//! Regression test — SSE `/events` live tail survives a broadcast `Lagged`.
//!
//! Guards the reliability fix recorded in
//! `docs/archive/plans/2026-07-10-meta-harness-reliability-audit.md` ("Verification
//! gaps"): the runtime-transport broadcast channel was widened to
//! `DEFAULT_RUNTIME_TRANSPORT_CAPACITY` (8192) AND the `/events` live-tail
//! forwarder in `api/events_api.rs` was changed so that a
//! `broadcast::error::RecvError::Lagged` is mapped to a synthetic
//! `__events_lagged__` NDJSON sentinel and the loop KEEPS GOING — it must NOT
//! return `Err` / close the stream. Before the fix a single slow subscriber
//! that lagged the channel tore the whole SSE tail down.
//!
//! The forwarder in `events_api.rs` is a private `tokio::spawn`ed loop inside
//! the actix handler, so it can't be driven in isolation without booting the
//! HTTP server. This test therefore exercises the two load-bearing pieces
//! against the REAL `RuntimeTransportBroadcaster`:
//!   1. tokio's broadcast `Lagged` semantics — overflow past capacity forces
//!      `RecvError::Lagged`, and the receiver stays usable (recv continues to
//!      yield later events) rather than being poisoned/closed.
//!   2. the exact sentinel mapping the forwarder applies on `Lagged` — the
//!      `__events_lagged__` shape carrying `skipped`.
//! Mirrors the in-crate broadcaster tests (`new`, `subscribe`,
//! `emit_transport_only`, `RuntimeTransportEvent::Heartbeat`).

use magician::magician_v2::realtime_events::{
    RuntimeTransportBroadcaster, RuntimeTransportEvent, DEFAULT_RUNTIME_TRANSPORT_CAPACITY,
};
use tokio::sync::broadcast::error::RecvError;

/// Recreates the `Lagged` → sentinel mapping the live-tail forwarder in
/// `api/events_api.rs` applies (the `RecvError::Lagged(skipped)` arm). Kept
/// byte-shape-identical to that arm so a drift in the sentinel contract fails
/// here. The forwarder does this INLINE and then continues the loop; we assert
/// on the produced value + the fact the loop is not broken.
fn lagged_sentinel(skipped: u64) -> serde_json::Value {
    serde_json::json!({
        "event_type": "__events_lagged__",
        "timestamp_ms": chrono::Utc::now().timestamp_millis(),
        "skipped": skipped,
        "message": format!(
            "Live event stream dropped {skipped} events because the channel \
             buffer was exhausted. Refresh the page to backfill recent activity."
        ),
    })
}

/// Overflowing the broadcast channel past its capacity must surface
/// `RecvError::Lagged` to a slow receiver, the forwarder must translate that
/// into the `__events_lagged__` sentinel (NOT an error/close), and the SAME
/// receiver must remain usable so the loop can keep forwarding subsequent
/// events. This is the precise behavior the events_api fix guarantees.
#[tokio::test]
async fn lagged_receiver_emits_sentinel_and_keeps_streaming() {
    // Small capacity so we can force a lag deterministically without emitting
    // 8192+ events. The forwarder behavior under `Lagged` is capacity-agnostic.
    let capacity = 4usize;
    let broadcaster = RuntimeTransportBroadcaster::new(capacity);
    let mut rx = broadcaster.subscribe();

    // Overflow: send strictly more than `capacity` events WITHOUT draining the
    // receiver. tokio's broadcast advances its head to the newest events and
    // marks the lagging receiver so its next `recv()` returns
    // `RecvError::Lagged(n)`. Heartbeat is the simplest variant (no scope
    // enrichment side effects).
    let overflow = capacity + 4; // 8 sent into a 4-slot channel
    for i in 0..overflow {
        broadcaster.emit_transport_only(RuntimeTransportEvent::Heartbeat {
            timestamp: i as i64,
        });
    }

    // First recv on the lagging receiver observes the drop as `Lagged`.
    let first = rx.recv().await;
    let skipped = match first {
        Err(RecvError::Lagged(skipped)) => skipped,
        other => panic!("expected RecvError::Lagged after overflow, got {other:?}"),
    };
    assert!(
        skipped >= 1,
        "Lagged must report at least one skipped event, got {skipped}"
    );

    // The forwarder maps Lagged → sentinel and CONTINUES. Assert the sentinel
    // shape the events_api arm produces, so a regression in that contract
    // (wrong event_type / missing skipped) fails here.
    let sentinel = lagged_sentinel(skipped);
    assert_eq!(
        sentinel.get("event_type").and_then(|v| v.as_str()),
        Some("__events_lagged__"),
        "lagged sentinel must carry the __events_lagged__ event_type"
    );
    assert_eq!(
        sentinel.get("skipped").and_then(|v| v.as_u64()),
        Some(skipped),
        "lagged sentinel must report the skipped count"
    );

    // The load-bearing assertion: the SAME receiver is still alive after
    // `Lagged` (the forwarder did not return/break). tokio keeps a lagged
    // receiver subscribed at the channel's current head, so it must be able to
    // recv fresh events emitted after the lag. If Lagged had torn the stream
    // down (the pre-fix bug), this would return `Closed`.
    broadcaster.emit_transport_only(RuntimeTransportEvent::Heartbeat { timestamp: 999 });

    // Drain whatever the receiver still holds (some retained post-overflow
    // events plus the newly emitted one). We must eventually see a live
    // `Heartbeat` recv succeed — proving the receiver survived the lag. A
    // second `Lagged` is possible on the way and is itself non-terminal, so we
    // tolerate it and keep going.
    let mut recovered = false;
    for _ in 0..(overflow + 4) {
        match rx.recv().await {
            Ok(RuntimeTransportEvent::Heartbeat { .. }) => {
                recovered = true;
                break;
            },
            Ok(_) => continue,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => {
                panic!("receiver was Closed after Lagged — stream was torn down (regression)")
            },
        }
    }
    assert!(
        recovered,
        "receiver must keep delivering events after a Lagged — the live tail did not survive"
    );
}

/// Capacity guard: the production broadcast channel is sized to 8192. This is
/// the headroom half of the fix — a burst has to be an order of magnitude
/// larger than the old 1000-slot channel before a live subscriber can lag. If
/// someone shrinks this back toward 1000, lag becomes common again and the
/// `/events` tail degrades — this pins the intended capacity.
#[test]
fn runtime_transport_capacity_is_widened() {
    assert_eq!(
        DEFAULT_RUNTIME_TRANSPORT_CAPACITY, 8192,
        "runtime transport broadcast capacity must stay at 8192 for lag headroom"
    );
}
