//! The one place an app-initiated capture start is serialized.
//!
//! The app control destination refuses a START while any capture is live. That
//! check and the start itself must be ONE critical section: probing the
//! registries and then awaiting `join_meeting_with_scope` (which launches a
//! browser) leaves a window in which two signed starts both observe "nothing
//! live" and both proceed — two rooms' microphones open, which is precisely the
//! outcome the refusal exists to prevent.
//!
//! Scope of the guarantee, stated exactly: this serializes **app-initiated**
//! starts against each other. A first-party `/meetings/listen` or `/meetings/join`
//! does not take the reservation, by design — the operator drives that path
//! directly and the platform does not second-guess it. So the invariant held
//! here is "an app never opens the second capture", not "at most one capture
//! exists".

use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;
use tokio::sync::{Mutex, OwnedMutexGuard};

static CAPTURE_START: Lazy<Arc<Mutex<()>>> = Lazy::new(|| Arc::new(Mutex::new(())));

/// Longest a start will wait for the reservation.
///
/// The holder is awaiting a real browser launch, so the wait is legitimately
/// seconds. But an unbounded wait means one wedged join blocks every later
/// app-initiated start for the life of the process with no diagnostic — the
/// caller gets a refusal it can explain instead.
const RESERVATION_WAIT: Duration = Duration::from_secs(45);

/// Hold this for the whole probe-then-start sequence. Dropping it releases the
/// next waiter, which re-probes and sees the session this one created.
///
/// `Err` means another start is still in flight past the wait bound; the caller
/// must refuse rather than proceed, because proceeding is exactly the double
/// start the reservation exists to prevent.
pub async fn reserve_app_capture_start() -> Result<OwnedMutexGuard<()>, ReservationTimeout> {
    tokio::time::timeout(RESERVATION_WAIT, Arc::clone(&CAPTURE_START).lock_owned())
        .await
        .map_err(|_| ReservationTimeout)
}

/// Another capture start held the reservation past [`RESERVATION_WAIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationTimeout;

impl std::fmt::Display for ReservationTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("another capture start is still in flight")
    }
}

impl std::error::Error for ReservationTimeout {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_reservation_is_exclusive_and_released_on_drop() {
        let first = reserve_app_capture_start().await.expect("uncontended");
        assert!(
            Arc::clone(&CAPTURE_START).try_lock_owned().is_err(),
            "a held reservation excludes a second start"
        );
        drop(first);
        let second = Arc::clone(&CAPTURE_START).try_lock_owned();
        assert!(
            second.is_ok(),
            "dropping the reservation admits the next start"
        );
    }
}
