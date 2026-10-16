//! Sequenced replay hub for the realtime transport.
//!
//! Every event that reaches the shared broadcast ring is also stamped here
//! with a monotonic `seq` and kept in a bounded replay ring, so a realtime
//! client that reconnects can ask for exactly the events it missed
//! (`resume_epoch` + `resume_seq`) instead of losing them.
//!
//! The hub sits *beside* the existing `broadcast::Sender<RuntimeTransportEvent>`
//! rather than replacing its payload type: dozens of internal subscribers
//! (transport log writer, `/v3/events`, chat fan-out, surfaces) consume plain
//! `RuntimeTransportEvent`s and never need a sequence number. Only the
//! realtime WebSocket subscribes to the sequenced channel. The retained copy
//! is shared through an `Arc` between the ring and the sequenced channel, so
//! each broadcast costs one extra clone of the event, not one per subscriber.
//!
//! Ordering: `seq` assignment, the ring push and the sequenced send happen
//! under one mutex, and [`EventHub::subscribe`] takes the same mutex while it
//! subscribes and snapshots the ring. A subscriber therefore never misses an
//! event between its replay snapshot and its live receiver, and never sees an
//! event in both (the WebSocket still dedupes by `seq` as a second guard).
//!
//! The epoch is one id per hub. Production builds exactly one
//! `RuntimeTransportBroadcaster` (see `magician_core/builder.rs`), so the
//! epoch is the process boot id and `seq` is process-wide. Keeping the counter
//! per hub (not a `static`) keeps each hub's ring contiguous, which the
//! "ring still holds `n + 1`" resume check relies on, and keeps tests that
//! build their own broadcaster independent of each other.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use tokio::sync::broadcast;
use uuid::Uuid;

use super::{
    clone_hitl_lifecycle_event_iteratively, discard_hitl_lifecycle_event_iteratively,
    RuntimeTransportEvent,
};

/// Number of most recent sequenced events kept for resume.
pub const EVENT_HUB_REPLAY_CAPACITY: usize = 8192;

/// One broadcast event with the sequence number it was published under.
#[derive(Clone, Debug)]
pub struct SequencedTransportEvent {
    /// Monotonic per hub (per process in production), starting at 1.
    pub seq: u64,
    pub event: Arc<RuntimeTransportEvent>,
}

/// Why a realtime client must re-read its state instead of trusting the
/// event stream it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubResyncReason {
    /// The requested resume point is no longer (or never was) in the ring.
    Gap,
    /// The subscriber fell behind live delivery and events were dropped.
    Lagged,
    /// The requested epoch is not this process's epoch (server restarted).
    Epoch,
}

impl HubResyncReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Lagged => "lagged",
            Self::Epoch => "epoch",
        }
    }
}

/// Result of matching a client's resume request against the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubResumeOutcome {
    /// No resume was requested.
    Fresh,
    /// The resume was satisfied; `replay` holds every event after it.
    Resumed,
    /// The resume could not be satisfied; the client must resync.
    Resync(HubResyncReason),
}

impl HubResumeOutcome {
    pub fn resumed(self) -> bool {
        matches!(self, Self::Resumed)
    }

    pub fn resync_reason(self) -> Option<HubResyncReason> {
        match self {
            Self::Resync(reason) => Some(reason),
            _ => None,
        }
    }
}

/// A live sequenced subscription plus the replay that precedes it.
#[derive(Debug)]
pub struct HubSubscription {
    pub epoch: Arc<str>,
    /// The last `seq` assigned when the subscription was taken. Every event
    /// on `receiver` has a larger `seq`.
    pub head_seq: u64,
    pub outcome: HubResumeOutcome,
    /// Events with `seq > resume_seq`, oldest first. Empty unless resumed.
    /// Not scope-filtered: the caller applies its own visibility rules.
    pub replay: Vec<SequencedTransportEvent>,
    pub receiver: broadcast::Receiver<SequencedTransportEvent>,
}

impl HubSubscription {
    /// The highest `seq` the subscriber has accounted for once the replay is
    /// delivered: the resume point when resumed, the head otherwise.
    pub fn initial_seen_seq(&self, resume_seq: Option<u64>) -> u64 {
        match (self.outcome, resume_seq) {
            (HubResumeOutcome::Resumed, Some(after)) => after,
            _ => self.head_seq,
        }
    }
}

struct EventHubRing {
    events: VecDeque<SequencedTransportEvent>,
    capacity: usize,
}

/// Sequencer, replay ring and sequenced broadcast channel.
#[derive(Clone)]
pub struct EventHub {
    epoch: Arc<str>,
    head: Arc<AtomicU64>,
    ring: Arc<Mutex<EventHubRing>>,
    sender: broadcast::Sender<SequencedTransportEvent>,
}

impl std::fmt::Debug for EventHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventHub")
            .field("epoch", &self.epoch)
            .field("head_seq", &self.head_seq())
            .finish_non_exhaustive()
    }
}

impl EventHub {
    /// `channel_capacity` sizes the live sequenced channel (same as the plain
    /// transport channel); `replay_capacity` sizes the resume ring.
    pub fn new(channel_capacity: usize, replay_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(channel_capacity.max(1));
        let replay_capacity = replay_capacity.max(1);
        Self {
            epoch: Arc::from(Uuid::new_v4().to_string()),
            head: Arc::new(AtomicU64::new(0)),
            ring: Arc::new(Mutex::new(EventHubRing {
                events: VecDeque::new(),
                capacity: replay_capacity,
            })),
            sender,
        }
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The last assigned `seq` (0 before the first event).
    pub fn head_seq(&self) -> u64 {
        self.head.load(Ordering::SeqCst)
    }

    fn lock_ring(&self) -> MutexGuard<'_, EventHubRing> {
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Assign the next `seq` to `event`, retain it in the ring and send it to
    /// sequenced subscribers. Returns the assigned `seq`.
    pub fn publish(&self, event: &RuntimeTransportEvent) -> u64 {
        // HITL lifecycle events can carry a caller-supplied JSON schema;
        // clone those without recursion, as every other retained copy does.
        let retained = Arc::new(
            clone_hitl_lifecycle_event_iteratively(event).unwrap_or_else(|| event.clone()),
        );
        let (seq, evicted) = {
            let mut ring = self.lock_ring();
            let seq = self.head.fetch_add(1, Ordering::SeqCst) + 1;
            let item = SequencedTransportEvent {
                seq,
                event: retained,
            };
            let evicted = if ring.events.len() >= ring.capacity {
                ring.events.pop_front()
            } else {
                None
            };
            ring.events.push_back(item.clone());
            // No receivers is not an error: the ring still serves resumes.
            let _ = self.sender.send(item);
            (seq, evicted)
        };
        // Drop outside the lock; an evicted HITL request is discarded
        // iteratively when this ring held the last reference.
        if let Some(evicted) = evicted {
            if let Ok(event) = Arc::try_unwrap(evicted.event) {
                discard_hitl_lifecycle_event_iteratively(event);
            }
        }
        seq
    }

    /// Subscribe to the sequenced channel and, when asked, collect the replay
    /// for a resume. Subscribing and snapshotting happen under the publish
    /// lock, so no event can fall between the replay and the receiver.
    pub fn subscribe(
        &self,
        resume_epoch: Option<&str>,
        resume_seq: Option<u64>,
    ) -> HubSubscription {
        let ring = self.lock_ring();
        let receiver = self.sender.subscribe();
        let head_seq = self.head.load(Ordering::SeqCst);
        let (outcome, replay) = match (resume_epoch, resume_seq) {
            (None, None) => (HubResumeOutcome::Fresh, Vec::new()),
            (Some(epoch), _) if epoch != &*self.epoch => {
                (HubResumeOutcome::Resync(HubResyncReason::Epoch), Vec::new())
            },
            // A seq without an epoch cannot be trusted to name this process.
            (None, Some(_)) => (HubResumeOutcome::Resync(HubResyncReason::Epoch), Vec::new()),
            (Some(_), None) => (HubResumeOutcome::Resync(HubResyncReason::Gap), Vec::new()),
            (Some(_), Some(after)) if after > head_seq => {
                (HubResumeOutcome::Resync(HubResyncReason::Gap), Vec::new())
            },
            (Some(_), Some(after)) if after == head_seq => (HubResumeOutcome::Resumed, Vec::new()),
            (Some(_), Some(after)) => match ring.events.front() {
                Some(oldest) if oldest.seq <= after.saturating_add(1) => (
                    HubResumeOutcome::Resumed,
                    ring.events
                        .iter()
                        .filter(|item| item.seq > after)
                        .cloned()
                        .collect(),
                ),
                _ => (HubResumeOutcome::Resync(HubResyncReason::Gap), Vec::new()),
            },
        };
        drop(ring);
        HubSubscription {
            epoch: Arc::clone(&self.epoch),
            head_seq,
            outcome,
            replay,
            receiver,
        }
    }

    /// Number of events currently retained for resume.
    pub fn retained_len(&self) -> usize {
        self.lock_ring().events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat(timestamp: i64) -> RuntimeTransportEvent {
        RuntimeTransportEvent::Heartbeat { timestamp }
    }

    fn timestamps(items: &[SequencedTransportEvent]) -> Vec<i64> {
        items
            .iter()
            .map(|item| match item.event.as_ref() {
                RuntimeTransportEvent::Heartbeat { timestamp } => *timestamp,
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    #[test]
    fn seq_is_monotonic_and_starts_at_one() {
        let hub = EventHub::new(16, 16);
        assert_eq!(hub.head_seq(), 0);
        let seqs: Vec<u64> = (0..5).map(|i| hub.publish(&heartbeat(i))).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
        assert_eq!(hub.head_seq(), 5);
    }

    #[test]
    fn seq_is_unique_and_ring_ordered_under_concurrent_publishers() {
        let hub = EventHub::new(64, 10_000);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let hub = hub.clone();
                std::thread::spawn(move || {
                    (0..500)
                        .map(|i| hub.publish(&heartbeat(t * 1000 + i)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<u64> = threads
            .into_iter()
            .flat_map(|handle| handle.join().expect("publisher thread"))
            .collect();
        all.sort_unstable();
        let expected: Vec<u64> = (1..=4000).collect();
        assert_eq!(all, expected, "every seq assigned exactly once");

        let sub = hub.subscribe(Some(hub.epoch()), Some(0));
        assert_eq!(sub.outcome, HubResumeOutcome::Resumed);
        let ring_seqs: Vec<u64> = sub.replay.iter().map(|item| item.seq).collect();
        assert_eq!(ring_seqs, expected, "ring holds events in seq order");
    }

    #[test]
    fn fresh_subscription_has_no_replay_and_starts_at_head() {
        let hub = EventHub::new(16, 16);
        hub.publish(&heartbeat(1));
        hub.publish(&heartbeat(2));
        let sub = hub.subscribe(None, None);
        assert_eq!(sub.outcome, HubResumeOutcome::Fresh);
        assert!(sub.replay.is_empty());
        assert_eq!(sub.head_seq, 2);
        assert_eq!(sub.initial_seen_seq(None), 2);
    }

    #[test]
    fn resume_replays_exactly_the_gap() {
        let hub = EventHub::new(16, 16);
        for i in 1..=6 {
            hub.publish(&heartbeat(i));
        }
        let sub = hub.subscribe(Some(hub.epoch()), Some(3));
        assert_eq!(sub.outcome, HubResumeOutcome::Resumed);
        assert_eq!(
            sub.replay.iter().map(|item| item.seq).collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
        assert_eq!(timestamps(&sub.replay), vec![4, 5, 6]);
        assert_eq!(sub.initial_seen_seq(Some(3)), 3);
    }

    #[test]
    fn resume_at_head_is_resumed_with_nothing_to_replay() {
        let hub = EventHub::new(16, 16);
        hub.publish(&heartbeat(1));
        let sub = hub.subscribe(Some(hub.epoch()), Some(1));
        assert_eq!(sub.outcome, HubResumeOutcome::Resumed);
        assert!(sub.replay.is_empty());
    }

    #[test]
    fn resume_past_the_ring_is_a_gap() {
        let hub = EventHub::new(16, 4);
        for i in 1..=10 {
            hub.publish(&heartbeat(i));
        }
        assert_eq!(hub.retained_len(), 4); // seqs 7..=10
        let gap = hub.subscribe(Some(hub.epoch()), Some(5));
        assert_eq!(gap.outcome, HubResumeOutcome::Resync(HubResyncReason::Gap));
        assert!(gap.replay.is_empty());
        // The oldest retained event is exactly `n + 1`: still resumable.
        let edge = hub.subscribe(Some(hub.epoch()), Some(6));
        assert_eq!(edge.outcome, HubResumeOutcome::Resumed);
        assert_eq!(
            edge.replay.iter().map(|item| item.seq).collect::<Vec<_>>(),
            vec![7, 8, 9, 10]
        );
        // A seq from the future is not trusted either.
        let future = hub.subscribe(Some(hub.epoch()), Some(99));
        assert_eq!(
            future.outcome,
            HubResumeOutcome::Resync(HubResyncReason::Gap)
        );
    }

    #[test]
    fn epoch_mismatch_resyncs() {
        let hub = EventHub::new(16, 16);
        hub.publish(&heartbeat(1));
        let other = EventHub::new(16, 16);
        assert_ne!(hub.epoch(), other.epoch());
        let sub = hub.subscribe(Some(other.epoch()), Some(0));
        assert_eq!(
            sub.outcome,
            HubResumeOutcome::Resync(HubResyncReason::Epoch)
        );
        assert!(sub.replay.is_empty());
        let no_epoch = hub.subscribe(None, Some(0));
        assert_eq!(
            no_epoch.outcome,
            HubResumeOutcome::Resync(HubResyncReason::Epoch)
        );
        let no_seq = hub.subscribe(Some(hub.epoch()), None);
        assert_eq!(
            no_seq.outcome,
            HubResumeOutcome::Resync(HubResyncReason::Gap)
        );
    }

    #[tokio::test]
    async fn events_published_after_subscribe_reach_the_receiver_not_the_replay() {
        let hub = EventHub::new(16, 16);
        hub.publish(&heartbeat(1));
        hub.publish(&heartbeat(2));
        let mut sub = hub.subscribe(Some(hub.epoch()), Some(1));
        hub.publish(&heartbeat(3));
        assert_eq!(
            sub.replay.iter().map(|item| item.seq).collect::<Vec<_>>(),
            vec![2]
        );
        let live = sub.receiver.recv().await.expect("live event");
        assert_eq!(live.seq, 3);
        assert!(
            sub.receiver.try_recv().is_err(),
            "no duplicate of the replay"
        );
    }

    #[tokio::test]
    async fn broadcaster_stamps_every_broadcast_event() {
        let broadcaster = super::super::RuntimeTransportBroadcaster::new(16);
        let mut sub = broadcaster.subscribe_event_hub(None, None);
        broadcaster.emit(heartbeat(1));
        broadcaster.emit(heartbeat(2));
        let first = sub.receiver.try_recv().expect("first sequenced event");
        let second = sub.receiver.try_recv().expect("second sequenced event");
        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(broadcaster.event_hub_head_seq(), 2);
    }
}
