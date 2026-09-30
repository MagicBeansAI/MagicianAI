//! MUIJ Event Coalescer — merges rapid-fire `MuijDelta::Upsert` deltas
//! per agent within a configurable time window before broadcast.
//!
//! - **Coalescable Upsert** (`coalescable: true`): merged by `component_id`
//!   within the window (last write wins). Used for latest-value components
//!   like Gauge.
//! - **Non-coalescable Upsert** (`coalescable: false`): passed through
//!   immediately — every entry matters for log-style components like
//!   TerminalTransient (R49).
//! - **Remove**: passed through immediately. Evicts any pending coalescable
//!   upsert for the same `component_id` to prevent resurrection after
//!   window flush (R87).
//! - **Reorder**: passed through immediately after flushing the target
//!   agent's pending upserts to preserve ordering semantics (R56).

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use tracing::{debug, warn};

use crate::magician_v2::gaui::MuijDelta;

/// R420: Maximum number of distinct agents tracked in the pending map.
/// Prevents unbounded growth when many agents send coalescable deltas
/// within the same window. Generous limit — 10,000 agents with pending
/// upserts simultaneously is well beyond expected production cardinality.
const MAX_PENDING_AGENTS: usize = 10_000;

/// Input to the coalescer: agent_id, delta, coalescable flag.
pub type CoalescerInput = (String, MuijDelta, bool);

/// Batched output: agent_id → coalesced deltas.
pub type CoalescedBatch = (String, Vec<MuijDelta>);

pub struct MuijCoalescer {
    window: Duration,
}

impl MuijCoalescer {
    pub fn new(window: Duration) -> Self {
        Self { window }
    }

    /// Default coalescing window (200ms).
    pub fn default_window() -> Duration {
        Duration::from_millis(200)
    }

    /// Coalesce upsert deltas per agent within the configured window.
    ///
    /// - **Coalescable Upsert**: merge by `component_id` (last write wins)
    /// - **Non-coalescable Upsert**: forward immediately (R49)
    /// - **Remove / Reorder**: forward immediately; Reorder flushes first (R56)
    ///
    /// Runs until `rx` is closed.
    pub async fn run(
        &self,
        mut rx: mpsc::Receiver<CoalescerInput>,
        tx: mpsc::Sender<CoalescedBatch>,
    ) {
        // Per-agent pending upserts: agent_id → (component_id → delta)
        let mut pending: HashMap<String, HashMap<String, MuijDelta>> = HashMap::new();
        let mut deadline: Option<Instant> = None;

        loop {
            // Resolve the deadline to a concrete Instant for sleep_until.
            // When no deadline is set, use a far-future instant (effectively infinite).
            let dl = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));

            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some((agent_id, delta, coalescable)) => {
                            match &delta {
                                MuijDelta::Upsert { component_id, .. } if coalescable => {
                                    // R420: Cap on distinct agents in pending map
                                    if pending.len() >= MAX_PENDING_AGENTS && !pending.contains_key(&agent_id) {
                                        warn!(
                                            agent_id = %agent_id,
                                            cap = MAX_PENDING_AGENTS,
                                            "Coalescer agent cap reached, dropping coalescable upsert (R420)"
                                        );
                                    } else {
                                        let agent_pending = pending
                                            .entry(agent_id.clone())
                                            .or_default();
                                        // R175: Cap per-agent pending to prevent unbounded growth
                                        // from agents with many unique component IDs.
                                        if agent_pending.len() >= 500 && !agent_pending.contains_key(component_id) {
                                            // R340: Include agent_id for multi-agent diagnosis
                                            warn!(
                                                agent_id = %agent_id,
                                                component_id = %component_id,
                                                "Coalescer pending cap reached, dropping coalescable upsert"
                                            );
                                        } else {
                                            agent_pending.insert(component_id.clone(), delta);
                                        }
                                        if deadline.is_none() {
                                            deadline = Some(Instant::now() + self.window);
                                        }
                                    }
                                }
                                MuijDelta::Upsert { .. } => {
                                    // Non-coalescable upsert (e.g. TerminalTransient) — pass through (R49)
                                    if tx.send((agent_id, vec![delta])).await.is_err() {
                                        warn!("[GAUI-COALESCER] Broadcast channel closed (non-coalescable upsert)");
                                        return;
                                    }
                                }
                                MuijDelta::Reorder { .. } => {
                                    // R101: Flush only the target agent's pending upserts before Reorder (not all agents)
                                    if let Some(agent_pending) = pending.remove(&agent_id) {
                                        // R377: Sort by component_id for deterministic order (consistent with timer flush)
                                        let mut deltas: Vec<MuijDelta> = agent_pending.into_values().collect();
                                        deltas.sort_by(|a, b| {
                                            let id_a = match a {
                                                MuijDelta::Upsert { component_id, .. } => component_id.as_str(),
                                                _ => "",
                                            };
                                            let id_b = match b {
                                                MuijDelta::Upsert { component_id, .. } => component_id.as_str(),
                                                _ => "",
                                            };
                                            id_a.cmp(id_b)
                                        });
                                        if !deltas.is_empty()
                                            && tx.send((agent_id.clone(), deltas)).await.is_err() {
                                                warn!("[GAUI-COALESCER] Broadcast channel closed (reorder flush)");
                                                return;
                                            }
                                    }
                                    // Reset deadline only if no other agents have pending work
                                    if pending.is_empty() {
                                        deadline = None;
                                    }
                                    if tx.send((agent_id, vec![delta])).await.is_err() {
                                        warn!("[GAUI-COALESCER] Broadcast channel closed (reorder passthrough)");
                                        return;
                                    }
                                }
                                MuijDelta::Remove { ref component_id } => {
                                    // Evict any pending upsert for this component to
                                    // prevent resurrection after window flush (R87).
                                    if let Some(agent_pending) = pending.get_mut(&agent_id) {
                                        agent_pending.remove(component_id);
                                        // R300: Clean up empty inner HashMap to prevent
                                        // wasted flush and timer wakeup.
                                        if agent_pending.is_empty() {
                                            pending.remove(&agent_id);
                                        }
                                    }
                                    // R365: Reset deadline when Remove empties the last
                                    // agent's pending map — mirrors Reorder arm (line 107).
                                    if pending.is_empty() {
                                        deadline = None;
                                    }
                                    // Pass-through immediately
                                    if tx.send((agent_id, vec![delta])).await.is_err() {
                                        warn!("[GAUI-COALESCER] Broadcast channel closed (remove passthrough)");
                                        return;
                                    }
                                }
                            }
                        }
                        None => {
                            // Channel closed — flush remaining and exit
                            Self::flush(&mut pending, &tx).await;
                            return;
                        }
                    }
                }

                _ = sleep_until(dl), if deadline.is_some() => {
                    Self::flush(&mut pending, &tx).await;
                    deadline = None;
                }
            }
        }
    }

    /// Flush all pending upserts as coalesced batches.
    ///
    /// R715: Batches are sorted by `agent_id` for deterministic broadcast order.
    /// This means alphabetically-earlier agents are always broadcast first within
    /// a single flush window — this is deliberate for reproducibility, not
    /// temporal ordering.
    async fn flush(
        pending: &mut HashMap<String, HashMap<String, MuijDelta>>,
        tx: &mpsc::Sender<CoalescedBatch>,
    ) {
        // R139: Collect and sort by agent_id for deterministic broadcast order
        let mut batches: Vec<(String, HashMap<String, MuijDelta>)> = pending.drain().collect();
        batches.sort_by(|a, b| a.0.cmp(&b.0));
        for (agent_id, upserts) in batches {
            // R366: Sort within-agent deltas by component_id for deterministic
            // ordering. HashMap::into_values() has arbitrary iteration order;
            // without this sort, downstream apply_delta_to_document sees
            // components in different order across flush calls.
            let mut deltas: Vec<MuijDelta> = upserts.into_values().collect();
            deltas.sort_by(|a, b| {
                let id_a = match a {
                    MuijDelta::Upsert { component_id, .. } => component_id.as_str(),
                    MuijDelta::Remove { component_id } => component_id.as_str(),
                    MuijDelta::Reorder { .. } => "",
                };
                let id_b = match b {
                    MuijDelta::Upsert { component_id, .. } => component_id.as_str(),
                    MuijDelta::Remove { component_id } => component_id.as_str(),
                    MuijDelta::Reorder { .. } => "",
                };
                id_a.cmp(id_b)
            });
            debug!(
                agent_id = %agent_id,
                count = deltas.len(),
                "Flushing coalesced upserts"
            );
            if tx.send((agent_id.clone(), deltas)).await.is_err() {
                // R632: Log which agent's batch was lost when downstream closes
                // mid-flush. Remaining batches in this flush are also dropped.
                warn!(
                    agent_id = %agent_id,
                    "[GAUI-COALESCER] Broadcast channel closed during flush — remaining batches dropped (R632)"
                );
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn two_coalescable_upserts_same_component_coalesce_to_one() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(50));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Send two coalescable upserts for same component_id
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.3}),
                },
                true, // coalescable
            ))
            .await
            .unwrap();
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.7}),
                },
                true, // coalescable
            ))
            .await
            .unwrap();

        // Close sender to trigger flush
        drop(in_tx);
        handle.await.unwrap();

        let (agent_id, deltas) = out_rx.recv().await.unwrap();
        assert_eq!(agent_id, "agent-1");
        assert_eq!(
            deltas.len(),
            1,
            "two coalescable upserts for same component_id should coalesce to one"
        );

        // Last write wins
        match &deltas[0] {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(data["fill"], json!(0.7));
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn non_coalescable_upserts_pass_through_immediately() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(200));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Send two non-coalescable upserts (TerminalTransient-style)
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "terminal-1".into(),
                    data: json!({"line": "line-1", "seq": 0}),
                },
                false, // NOT coalescable — log line must not be dropped (R49)
            ))
            .await
            .unwrap();
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "terminal-1".into(),
                    data: json!({"line": "line-2", "seq": 1}),
                },
                false,
            ))
            .await
            .unwrap();

        // Both should arrive immediately (within 50ms, well before 200ms window)
        let r1 = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(
            r1.is_ok(),
            "first non-coalescable upsert should pass through immediately"
        );
        let (_, deltas1) = r1.unwrap().unwrap();
        assert_eq!(deltas1.len(), 1);

        let r2 = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(
            r2.is_ok(),
            "second non-coalescable upsert should pass through immediately"
        );
        let (_, deltas2) = r2.unwrap().unwrap();
        assert_eq!(deltas2.len(), 1);

        // Both lines preserved (not coalesced)
        match (&deltas1[0], &deltas2[0]) {
            (MuijDelta::Upsert { data: d1, .. }, MuijDelta::Upsert { data: d2, .. }) => {
                assert_eq!(d1["line"], json!("line-1"));
                assert_eq!(d2["line"], json!("line-2"));
            },
            other => panic!("expected two Upserts, got {:?}", other),
        }

        drop(in_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn upserts_different_component_ids_stay_separate() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(50));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.3}),
                },
                true,
            ))
            .await
            .unwrap();
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-2".into(),
                    data: json!({"fill": 0.5}),
                },
                true,
            ))
            .await
            .unwrap();

        drop(in_tx);
        handle.await.unwrap();

        let (agent_id, deltas) = out_rx.recv().await.unwrap();
        assert_eq!(agent_id, "agent-1");
        assert_eq!(
            deltas.len(),
            2,
            "different component_ids should not coalesce"
        );
    }

    #[tokio::test]
    async fn remove_passes_through_immediately() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(200));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Remove {
                    component_id: "gauge-1".into(),
                },
                false, // coalescable flag ignored for Remove
            ))
            .await
            .unwrap();

        // Should receive immediately without waiting for window
        let result = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(result.is_ok(), "Remove should pass through immediately");

        let (agent_id, deltas) = result.unwrap().unwrap();
        assert_eq!(agent_id, "agent-1");
        assert_eq!(deltas.len(), 1);
        assert!(matches!(&deltas[0], MuijDelta::Remove { .. }));

        drop(in_tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn window_flushes_after_duration() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(50));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.5}),
                },
                true,
            ))
            .await
            .unwrap();

        // Yield to let the coalescer receive the message and set its deadline
        tokio::task::yield_now().await;

        // Advance mock clock past the 50ms coalescing window
        tokio::time::advance(Duration::from_millis(60)).await;

        // recv() will yield to the coalescer task, which processes the fired timer
        let (agent_id, deltas) = out_rx.recv().await.unwrap();
        assert_eq!(agent_id, "agent-1");
        assert_eq!(deltas.len(), 1);

        drop(in_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn reorder_flushes_pending_before_passthrough() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(200));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Send a coalescable upsert (pending in window)
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.5}),
                },
                true,
            ))
            .await
            .unwrap();

        // Send a Reorder — should flush the pending upsert first (R56)
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Reorder {
                    ids: vec!["gauge-1".into(), "terminal-1".into()],
                },
                false,
            ))
            .await
            .unwrap();

        // First batch: flushed upsert
        let r1 = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(
            r1.is_ok(),
            "pending upsert should be flushed before Reorder"
        );
        let (_, deltas1) = r1.unwrap().unwrap();
        assert!(matches!(&deltas1[0], MuijDelta::Upsert { .. }));

        // Second batch: the Reorder
        let r2 = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(r2.is_ok(), "Reorder should follow immediately after flush");
        let (_, deltas2) = r2.unwrap().unwrap();
        assert!(matches!(&deltas2[0], MuijDelta::Reorder { .. }));

        drop(in_tx);
        handle.await.unwrap();
    }

    // R436: Remove evicts pending upsert for same component, preventing resurrection
    #[tokio::test]
    async fn remove_evicts_pending_upsert_for_same_component() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(200));
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Send a coalescable upsert (pending in window)
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.5}),
                },
                true,
            ))
            .await
            .unwrap();

        // Send a Remove for the same component — should evict the pending upsert
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Remove {
                    component_id: "gauge-1".into(),
                },
                false,
            ))
            .await
            .unwrap();

        // Remove passes through immediately
        let r1 = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        assert!(r1.is_ok(), "Remove should pass through immediately");
        let (_, deltas1) = r1.unwrap().unwrap();
        assert!(matches!(&deltas1[0], MuijDelta::Remove { .. }));

        // Close and flush — should NOT see the evicted upsert
        drop(in_tx);
        handle.await.unwrap();

        // Try to receive any remaining — should be None (no pending upserts)
        let remaining = tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await;
        match remaining {
            Ok(None) | Err(_) => {}, // Expected: no more messages
            Ok(Some((_, deltas))) => {
                // If we do get a batch, it should not contain the removed component
                for d in &deltas {
                    if let MuijDelta::Upsert { component_id, .. } = d {
                        assert_ne!(
                            component_id, "gauge-1",
                            "evicted upsert should NOT be resurrected after Remove (R436)"
                        );
                    }
                }
            },
        }
    }

    // R437: Per-agent pending cap (500 unique components)
    #[tokio::test]
    async fn per_agent_pending_cap_enforced() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(200));
        let (in_tx, in_rx) = mpsc::channel(1024);
        let (out_tx, mut out_rx) = mpsc::channel(1024);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Send 501 coalescable upserts with unique component IDs
        for i in 0..501 {
            in_tx
                .send((
                    "agent-cap".into(),
                    MuijDelta::Upsert {
                        component_id: format!("comp-{i}"),
                        data: json!({"fill": i}),
                    },
                    true,
                ))
                .await
                .unwrap();
        }

        // Close and flush
        drop(in_tx);
        handle.await.unwrap();

        // Collect all flushed deltas
        let mut total_deltas = 0;
        while let Ok(Some((_, deltas))) =
            tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await
        {
            total_deltas += deltas.len();
        }

        // Should have at most 500 (the 501st was dropped by the cap)
        assert!(
            total_deltas <= 500,
            "per-agent pending cap should limit to 500, got {}",
            total_deltas
        );
        assert!(
            total_deltas >= 499,
            "should have at least 499 deltas (cap enforcement is per new component), got {}",
            total_deltas
        );
    }

    // R603: Multi-agent interleaved batching — each agent's deltas coalesce independently
    #[tokio::test]
    async fn multi_agent_interleaved_coalesce_independently() {
        let coalescer = MuijCoalescer::new(Duration::from_millis(50));
        let (in_tx, in_rx) = mpsc::channel(32);
        let (out_tx, mut out_rx) = mpsc::channel(32);

        let handle = tokio::spawn(async move {
            coalescer.run(in_rx, out_tx).await;
        });

        // Interleave coalescable upserts for two different agents
        // Agent-1: gauge fill 0.3, then 0.7 (should coalesce to 0.7)
        // Agent-2: gauge fill 0.4, then 0.8 (should coalesce to 0.8)
        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.3}),
                },
                true,
            ))
            .await
            .unwrap();

        in_tx
            .send((
                "agent-2".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-2".into(),
                    data: json!({"fill": 0.4}),
                },
                true,
            ))
            .await
            .unwrap();

        in_tx
            .send((
                "agent-1".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-1".into(),
                    data: json!({"fill": 0.7}),
                },
                true,
            ))
            .await
            .unwrap();

        in_tx
            .send((
                "agent-2".into(),
                MuijDelta::Upsert {
                    component_id: "gauge-2".into(),
                    data: json!({"fill": 0.8}),
                },
                true,
            ))
            .await
            .unwrap();

        // Close to trigger flush
        drop(in_tx);
        handle.await.unwrap();

        // Collect all output batches
        let mut agent_batches: HashMap<String, Vec<MuijDelta>> = HashMap::new();
        while let Ok(Some((agent_id, deltas))) =
            tokio::time::timeout(Duration::from_millis(50), out_rx.recv()).await
        {
            agent_batches.entry(agent_id).or_default().extend(deltas);
        }

        // Each agent should have exactly 1 coalesced delta
        assert_eq!(
            agent_batches.get("agent-1").map(|d| d.len()),
            Some(1),
            "agent-1 should have 1 coalesced delta"
        );
        assert_eq!(
            agent_batches.get("agent-2").map(|d| d.len()),
            Some(1),
            "agent-2 should have 1 coalesced delta"
        );

        // Verify last-write-wins for each agent
        match &agent_batches["agent-1"][0] {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(data["fill"], json!(0.7), "agent-1 should keep last value");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
        match &agent_batches["agent-2"][0] {
            MuijDelta::Upsert { data, .. } => {
                assert_eq!(data["fill"], json!(0.8), "agent-2 should keep last value");
            },
            other => panic!("expected Upsert, got {:?}", other),
        }
    }
}
