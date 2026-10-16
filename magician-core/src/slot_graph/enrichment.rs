use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info_span, Instrument};

use super::types::{SlotRecord, SlotType};

/// Context supplied to enrichers when processing a slot.
#[derive(Debug, Clone)]
pub struct EnrichmentContext {
    /// Position of the slot in the current batch.
    pub slot_index: usize,
    /// Identifier of the slot being enriched.
    pub slot_id: String,
    /// Slot type being enriched.
    pub slot_type: SlotType,
}

impl EnrichmentContext {
    fn new(slot_index: usize, slot: &SlotRecord) -> Self {
        Self {
            slot_index,
            slot_id: slot.id.clone(),
            slot_type: slot.slot_type.clone(),
        }
    }
}

/// Result returned by an enricher after attempting to enrich a slot.
#[derive(Debug, Clone)]
pub struct EnrichmentOutcome {
    /// True when the enricher modified the slot.
    pub changed: bool,
    /// Optional descriptive note about the enrichment.
    pub notes: Option<String>,
}

impl EnrichmentOutcome {
    /// Outcome indicating no changes were made.
    pub fn unchanged() -> Self {
        Self {
            changed: false,
            notes: None,
        }
    }

    /// Outcome indicating the slot was updated.
    pub fn updated<S: Into<String>>(notes: Option<S>) -> Self {
        Self {
            changed: true,
            notes: notes.map(Into::into),
        }
    }
}

/// Confidence delta emitted during enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceUpdate {
    pub slot_id: String,
    pub old_confidence: f64,
    pub new_confidence: f64,
    pub enricher: String,
}

/// Trait that all deterministic slot enrichers must implement.
#[async_trait]
pub trait SlotEnricher: Send + Sync {
    /// Human-readable identifier used in logging and reporting.
    fn name(&self) -> &'static str;

    /// Slot types this enricher can operate on.
    ///
    /// Returning an empty slice signals the enricher is willing to see every
    /// slot regardless of type.
    fn supported_types(&self) -> &'static [SlotType];

    /// Execute enrichment logic in-place on the provided slot.
    async fn enrich(
        &self,
        slot: &mut SlotRecord,
        context: &EnrichmentContext,
    ) -> Result<EnrichmentOutcome>;
}

/// Aggregated metrics returned after an enrichment pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentSummary {
    /// Total slots provided to the pipeline.
    pub total_slots: usize,
    /// Number of enricher invocations (slot × enricher combinations attempted).
    pub invocations: usize,
    /// Number of enrichers that reported modifying a slot.
    pub enrichments_applied: usize,
    /// Number of distinct slots that were modified at least once.
    pub slots_changed: usize,
    /// Errors captured during execution.
    pub errors: Vec<EnrichmentError>,
    /// Confidence updates captured during enrichment.
    #[serde(default)]
    pub confidence_updates: Vec<ConfidenceUpdate>,
    changed_slot_ids: HashSet<String>,
}

impl EnrichmentSummary {
    fn record_invocation(&mut self) {
        self.invocations += 1;
    }

    fn record_error(&mut self, error: EnrichmentError) {
        self.errors.push(error);
    }

    fn finalize(mut self) -> Self {
        self.slots_changed = self.changed_slot_ids.len();
        self
    }

    fn new(total_slots: usize) -> Self {
        Self {
            total_slots,
            invocations: 0,
            enrichments_applied: 0,
            slots_changed: 0,
            errors: Vec::new(),
            confidence_updates: Vec::new(),
            changed_slot_ids: HashSet::new(),
        }
    }

    fn register_slot_change(&mut self, slot_id: &str) {
        if self.changed_slot_ids.insert(slot_id.to_string()) {
            self.slots_changed = self.changed_slot_ids.len();
        }
    }

    fn register_enrichment(&mut self, slot_id: &str) {
        self.enrichments_applied += 1;
        self.register_slot_change(slot_id);
    }

    fn record_confidence_update(
        &mut self,
        slot_id: &str,
        old_confidence: f64,
        new_confidence: f64,
        enricher: &str,
    ) {
        self.confidence_updates.push(ConfidenceUpdate {
            slot_id: slot_id.to_string(),
            old_confidence,
            new_confidence,
            enricher: enricher.to_string(),
        });
    }
}

impl Default for EnrichmentSummary {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Error captured during enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentError {
    pub slot_id: String,
    pub enricher: String,
    pub message: String,
}

/// Pipeline that runs deterministic enrichers sequentially.
pub struct EnrichmentPipeline {
    enrichers: Vec<Arc<dyn SlotEnricher>>,
}

impl EnrichmentPipeline {
    /// Construct a pipeline with the provided enrichers in execution order.
    pub fn new(enrichers: Vec<Arc<dyn SlotEnricher>>) -> Self {
        Self { enrichers }
    }

    /// Return number of enrichers configured in pipeline.
    pub fn enricher_count(&self) -> usize {
        self.enrichers.len()
    }

    /// Run the pipeline across the provided slots.
    pub async fn run(&self, slots: &mut [SlotRecord]) -> EnrichmentSummary {
        let mut summary = EnrichmentSummary::new(slots.len());

        for (index, slot) in slots.iter_mut().enumerate() {
            if self.enrichers.is_empty() {
                break;
            }

            let context = EnrichmentContext::new(index, slot);
            let span = info_span!(
                "slot_enrichment",
                slot_id = context.slot_id.as_str(),
                slot_type = ?context.slot_type
            );

            async {
                for enricher in &self.enrichers {
                    if !Self::supports(enricher.as_ref(), &slot.slot_type) {
                        continue;
                    }

                    summary.record_invocation();
                    let original_confidence = slot.confidence;

                    match enricher.enrich(slot, &context).await {
                        Ok(outcome) => {
                            if outcome.changed {
                                summary.register_enrichment(&context.slot_id);
                                if let Some(notes) = outcome.notes {
                                    debug!(
                                        "[MAGICIAN-V2-ENRICH] {} updated slot {}: {}",
                                        enricher.name(),
                                        context.slot_id,
                                        notes
                                    );
                                } else {
                                    debug!(
                                        "[MAGICIAN-V2-ENRICH] {} updated slot {}",
                                        enricher.name(),
                                        context.slot_id
                                    );
                                }
                            } else {
                                debug!(
                                    "[MAGICIAN-V2-ENRICH] {} made no changes to slot {}",
                                    enricher.name(),
                                    context.slot_id
                                );
                            }
                            if (slot.confidence - original_confidence).abs() > f64::EPSILON {
                                summary.record_confidence_update(
                                    &context.slot_id,
                                    original_confidence,
                                    slot.confidence,
                                    enricher.name(),
                                );
                            }
                        },
                        Err(err) => {
                            error!(
                                "[MAGICIAN-V2-ENRICH] {} failed for slot {}: {}",
                                enricher.name(),
                                context.slot_id,
                                err
                            );
                            summary.record_error(EnrichmentError {
                                slot_id: context.slot_id.clone(),
                                enricher: enricher.name().to_string(),
                                message: err.to_string(),
                            });
                        },
                    }
                }
            }
            .instrument(span)
            .await;
        }

        summary.finalize()
    }

    fn supports(enricher: &dyn SlotEnricher, slot_type: &SlotType) -> bool {
        let supported = enricher.supported_types();
        supported.is_empty() || supported.contains(slot_type)
    }
}

impl Default for EnrichmentPipeline {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use anyhow::anyhow;
    use serde_json::json;

    use crate::slot_graph::types::{ProvenanceRecord, ProvenanceSource};

    struct UppercaseEntityEnricher;

    #[async_trait]
    impl SlotEnricher for UppercaseEntityEnricher {
        fn name(&self) -> &'static str {
            "uppercase_entity"
        }

        fn supported_types(&self) -> &'static [SlotType] {
            &[SlotType::Entity]
        }

        async fn enrich(
            &self,
            slot: &mut SlotRecord,
            _context: &EnrichmentContext,
        ) -> Result<EnrichmentOutcome> {
            if let Some(name) = slot.value.get("name").and_then(|v| v.as_str()) {
                let upper = name.to_uppercase();
                if upper != name {
                    slot.value["name"] = json!(upper);
                    slot.provenance.push(ProvenanceRecord {
                        source: ProvenanceSource::DeterministicCheck,
                        timestamp: chrono::Utc::now(),
                    });
                    return Ok(EnrichmentOutcome::updated(Some("normalized entity name")));
                }
            }
            Ok(EnrichmentOutcome::unchanged())
        }
    }

    struct FailingEnricher;

    #[async_trait]
    impl SlotEnricher for FailingEnricher {
        fn name(&self) -> &'static str {
            "fails"
        }

        fn supported_types(&self) -> &'static [SlotType] {
            &[]
        }

        async fn enrich(
            &self,
            _slot: &mut SlotRecord,
            context: &EnrichmentContext,
        ) -> Result<EnrichmentOutcome> {
            Err(anyhow!("synthetic failure on slot {}", context.slot_id))
        }
    }

    fn make_slot(id: &str, value: serde_json::Value, slot_type: SlotType) -> SlotRecord {
        SlotRecord {
            id: id.to_string(),
            slot_type,
            value,
            confidence: 0.5,
            provenance: vec![],
            evidence_links: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn pipeline_updates_slots_and_tracks_metrics() {
        let enrichers: Vec<Arc<dyn SlotEnricher>> =
            vec![Arc::new(UppercaseEntityEnricher), Arc::new(FailingEnricher)];
        let pipeline = EnrichmentPipeline::new(enrichers);

        let mut slots = vec![
            make_slot("entity_1", json!({"name": "ada"}), SlotType::Entity),
            make_slot("status_1", json!({"value": "ready"}), SlotType::Status),
        ];

        let summary = pipeline.run(&mut slots).await;

        assert_eq!(summary.total_slots, 2);
        assert_eq!(summary.enrichments_applied, 1);
        assert_eq!(summary.slots_changed, 1);
        assert_eq!(
            summary.errors.len(),
            2,
            "failing enricher should error on each slot"
        );

        let entity_slot = &slots[0];
        assert_eq!(
            entity_slot.value.get("name").and_then(|v| v.as_str()),
            Some("ADA")
        );
        assert_eq!(entity_slot.provenance.len(), 1);
    }
}
