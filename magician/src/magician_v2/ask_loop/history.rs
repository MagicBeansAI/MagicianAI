use std::collections::{HashMap, HashSet};

use dashmap::DashMap;

use crate::magician_v2::{slot_graph::SlotRecord, state_tracker::StageContext};

/// Tracks clarification requests per workflow and stage so we only resend newly
/// blocking slots and clear confirmed ones.
#[derive(Default)]
pub struct ClarificationHistory {
    workflows: DashMap<String, StageHistory>,
}

impl ClarificationHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retain only slot records that are newly blocking for the given stage.
    /// Previously requested slots are filtered out, and any new entries are
    /// persisted so subsequent calls know they've been asked already.
    pub fn filter_new_slots(
        &self,
        workflow_id: &str,
        stage: StageContext,
        slots: &[SlotRecord],
    ) -> Vec<SlotRecord> {
        if slots.is_empty() {
            return Vec::new();
        }

        let mut entry = self
            .workflows
            .entry(workflow_id.to_string())
            .or_insert_with(StageHistory::default);

        entry.filter_new(stage, slots)
    }

    /// Remove any slots that are no longer reported as unresolved for the
    /// current stage. This keeps the history aligned with the confidence model.
    pub fn prune_resolved(
        &self,
        workflow_id: &str,
        stage: StageContext,
        unresolved_ids: &[String],
    ) {
        let Some(mut entry) = self.workflows.get_mut(workflow_id) else {
            return;
        };

        if unresolved_ids.is_empty() {
            entry.clear_stage(stage);
        } else {
            entry.prune(stage, unresolved_ids);
        }

        if entry.is_empty() {
            drop(entry);
            self.workflows.remove(workflow_id);
        }
    }

    /// Mark the supplied slots as resolved for the given stage. Any pending
    /// entries that match the slot identifiers are removed from the history.
    pub fn resolve(&self, workflow_id: &str, stage: StageContext, slots: &[SlotRecord]) {
        if slots.is_empty() {
            return;
        }

        let Some(mut entry) = self.workflows.get_mut(workflow_id) else {
            return;
        };

        entry.resolve(stage, slots);

        if entry.is_empty() {
            drop(entry);
            self.workflows.remove(workflow_id);
        }
    }
}

#[derive(Default, Clone)]
struct StageHistory {
    per_stage: HashMap<StageContext, StageClarification>,
}

impl StageHistory {
    fn filter_new(&mut self, stage: StageContext, slots: &[SlotRecord]) -> Vec<SlotRecord> {
        let stage_history = self.per_stage.entry(stage).or_default();
        stage_history.filter_new(slots)
    }

    fn prune(&mut self, stage: StageContext, unresolved_ids: &[String]) {
        if let Some(stage_history) = self.per_stage.get_mut(&stage) {
            stage_history.prune(unresolved_ids);
            if stage_history.is_empty() {
                self.per_stage.remove(&stage);
            }
        }
    }

    fn clear_stage(&mut self, stage: StageContext) {
        self.per_stage.remove(&stage);
    }

    fn resolve(&mut self, stage: StageContext, slots: &[SlotRecord]) {
        if let Some(stage_history) = self.per_stage.get_mut(&stage) {
            stage_history.resolve(slots);
            if stage_history.is_empty() {
                self.per_stage.remove(&stage);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.per_stage.is_empty()
    }
}

#[derive(Default, Clone)]
struct StageClarification {
    pending_slots: HashSet<String>,
}

impl StageClarification {
    fn filter_new(&mut self, slots: &[SlotRecord]) -> Vec<SlotRecord> {
        let mut fresh = Vec::new();
        for slot in slots {
            if self.pending_slots.insert(slot.id.clone()) {
                fresh.push(slot.clone());
            }
        }
        fresh
    }

    fn prune(&mut self, unresolved_ids: &[String]) {
        if self.pending_slots.is_empty() {
            return;
        }

        let unresolved: HashSet<&str> = unresolved_ids.iter().map(|id| id.as_str()).collect();
        self.pending_slots
            .retain(|slot_id| unresolved.contains(slot_id.as_str()));
    }

    fn resolve(&mut self, slots: &[SlotRecord]) {
        for slot in slots {
            self.pending_slots.remove(&slot.id);
        }
    }

    fn is_empty(&self) -> bool {
        self.pending_slots.is_empty()
    }
}
