//! Convert a durable resource deadline without starting a second clock.

use chrono::{DateTime, Duration, Utc};

pub(crate) fn absolute_deadline(
    root_started_at: DateTime<Utc>,
    expires_at_elapsed_ms: u64,
) -> Option<DateTime<Utc>> {
    root_started_at.checked_add_signed(Duration::try_milliseconds(
        i64::try_from(expires_at_elapsed_ms).ok()?,
    )?)
}
