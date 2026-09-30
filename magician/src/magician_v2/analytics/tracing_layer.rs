//! Tracing subscriber layer that forwards log events to the analytics event sink.
//!
//! This layer captures structured tracing events (info, warn, error) and emits
//! them as `AnalyticsEvent::log()` via the global `analytics::emit()` function.
//!
//! **Recursion guard**: Events with target starting with "analytics" are skipped
//! to prevent the analytics sink's own logging from feeding back into itself.

use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

/// Library targets whose INFO+ output is transport plumbing rather than
/// anything this runtime did.
///
/// Shared with
/// [`super::runtime_activity_layer::RuntimeActivityLayer`] deliberately:
/// two copies of this list would drift, and a target muted for the
/// analytics events table but live in the activity view would be a
/// difference nobody chose. Add here, and both layers agree.
pub const NOISY_TARGET_PREFIXES: &[&str] = &[
    "hyper",
    "rustls",
    "h2",
    "tungstenite",
    "tokio_tungstenite",
    "mio",
    "want",
];

/// Whether `target` belongs to a [`NOISY_TARGET_PREFIXES`] library.
///
/// Prefix match, so submodules (`hyper::client::conn`) are covered by the
/// crate-level entry.
pub fn is_noisy_target(target: &str) -> bool {
    NOISY_TARGET_PREFIXES
        .iter()
        .any(|prefix| target.starts_with(prefix))
}

/// A tracing layer that forwards log events to the analytics event sink.
///
/// Only captures WARN and above by default. Skips noisy library targets
/// and all `analytics::*` targets to prevent feedback loops.
pub struct AnalyticsTracingLayer;

impl<S: Subscriber> Layer<S> for AnalyticsTracingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let target = metadata.target();

        // Recursion guard: never capture our own analytics logging.
        if target.starts_with("analytics") {
            return;
        }

        // Capture INFO and above. DEBUG/TRACE are too noisy for the events table.
        let level = *metadata.level();
        if level > tracing::Level::INFO {
            return;
        }

        // Skip noisy library targets.
        if is_noisy_target(target) {
            return;
        }

        // Extract the message from the event's fields.
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let level_str = match level {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            tracing::Level::DEBUG => "debug",
            tracing::Level::TRACE => "trace",
        };

        super::emit(super::event_sink::AnalyticsEvent::log(
            level_str,
            &visitor.message,
            target,
        ));
    }
}

/// Visitor that extracts the `message` field from a tracing event.
///
/// Shared with [`super::runtime_activity_layer`] rather than copied, so
/// there is exactly one answer to "what part of a log event crosses a
/// process boundary". It captures `message` and nothing else — every
/// other structured field stays in the process. Widening this widens the
/// activity websocket payload too, which is a privacy decision, not a
/// convenience one.
#[derive(Default)]
pub struct MessageVisitor {
    pub message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            // Remove surrounding quotes from Debug formatting
            if self.message.starts_with('"') && self.message.ends_with('"') {
                self.message = self.message[1..self.message.len() - 1].to_string();
            }
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}
