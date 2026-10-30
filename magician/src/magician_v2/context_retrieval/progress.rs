//! Content-free context timing on the same scoped event rail as tool steps.
use super::{ContextRetrievalObservation, ContextStageKind, ContextStageState};
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use serde::Serialize;
use serde_json::json;
use std::{sync::Arc, time::Instant};

#[derive(Default, Serialize)]
pub struct ContextProgressCorrelation {
    pub chat_session_id: Option<String>,
    pub chat_turn_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
}

#[derive(Default, Serialize)]
pub struct ContextPreparationTiming {
    pub duration_ms: f64,
    pub memory_ms: Option<f64>,
    pub procedures_ms: Option<f64>,
    pub memory_incomplete: bool,
    pub procedures_incomplete: bool,
    pub context_items: usize,
    pub retrieval_backends: Vec<String>,
    pub partial: bool,
    pub cancelled: bool,
}

impl From<&ContextRetrievalObservation> for ContextPreparationTiming {
    fn from(observation: &ContextRetrievalObservation) -> Self {
        let mut timing = Self {
            duration_ms: observation.elapsed_ms,
            context_items: observation.contribution_count,
            retrieval_backends: observation.retrieval_backends.clone(),
            partial: observation.deadline_reached || observation.error_stage_count > 0,
            cancelled: observation.cancelled,
            ..Self::default()
        };
        for stage in &observation.stages {
            let completed = matches!(
                stage.status,
                ContextStageState::Completed | ContextStageState::Empty
            );
            match stage.stage {
                ContextStageKind::FastMemory | ContextStageKind::HybridMemory => {
                    timing.memory_incomplete |= !completed;
                    if completed {
                        timing.memory_ms =
                            Some(timing.memory_ms.unwrap_or(0.0).max(stage.elapsed_ms));
                    }
                },
                ContextStageKind::ReusableProcedures => {
                    timing.procedures_incomplete = !completed;
                    if completed {
                        timing.procedures_ms = Some(stage.elapsed_ms);
                    }
                },
                _ => {},
            }
        }
        timing
    }
}

/// Dropping a cancelled/failed preparation settles its row instead of leaving
/// a permanent running step. Only caller-owned correlation and timings leave.
pub struct ContextPreparationProgress {
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    principal: String,
    workspace: String,
    agent_id: String,
    correlation: ContextProgressCorrelation,
    phase_id: String,
    started: Instant,
    finished: bool,
}

impl ContextPreparationProgress {
    pub fn start(
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: Option<&str>,
        workspace: Option<&str>,
        agent_id: &str,
        correlation: ContextProgressCorrelation,
    ) -> Option<Self> {
        let (principal, workspace) = (principal?, workspace?);
        if principal.trim().is_empty()
            || workspace.trim().is_empty()
            || agent_id.trim().is_empty()
            || (correlation.task_id.is_none()
                && correlation.execution_id.is_none()
                && (correlation.chat_session_id.is_none() || correlation.chat_turn_id.is_none()))
        {
            return None;
        }
        let progress = Self {
            broadcaster: broadcaster?,
            principal: principal.into(),
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            correlation,
            phase_id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            finished: false,
        };
        progress.emit("context.retrieval.started", None);
        Some(progress)
    }

    pub fn finish(&mut self, timing: &ContextPreparationTiming) {
        if !self.finished {
            self.finished = true;
            self.emit("context.retrieval.completed", Some(timing));
        }
    }

    fn emit(&self, event: &str, timing: Option<&ContextPreparationTiming>) {
        let mut payload =
            serde_json::to_value(&self.correlation).expect("fixed correlation fields");
        let fields = payload.as_object_mut().expect("correlation is an object");
        fields.insert("phase_id".into(), json!(self.phase_id));
        fields.insert("label".into(), json!("Preparing context"));
        if let Some(timing) = timing {
            fields.insert("duration_ms".into(), json!(timing.duration_ms));
            fields.insert(
                "status".into(),
                json!(if timing.cancelled {
                    "cancelled"
                } else if timing.partial {
                    "partial"
                } else {
                    "completed"
                }),
            );
            fields.insert(
                "summary".into(),
                json!(if timing.cancelled {
                    "Context preparation cancelled".to_string()
                } else {
                    let methods = timing
                        .retrieval_backends
                        .iter()
                        .filter_map(|backend| match backend.as_str() {
                            "lancedb_hybrid" => Some("hybrid"),
                            "direct" | "direct_keyword" | "direct_fallback" => Some("keyword"),
                            "fast_snapshot" => Some("fast snapshot"),
                            _ => None,
                        })
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ");
                    let component = |label: &str, elapsed: Option<f64>, incomplete: bool| {
                        if incomplete {
                            format!("{label}: not completed")
                        } else if let Some(ms) = elapsed {
                            format!("{label}: {ms:.0} ms")
                        } else {
                            format!("{label}: not run")
                        }
                    };
                    format!(
                        "{:.0} ms total · {} · {}{}{}",
                        timing.duration_ms,
                        component("Memory search", timing.memory_ms, timing.memory_incomplete),
                        component(
                            "Procedures",
                            timing.procedures_ms,
                            timing.procedures_incomplete
                        ),
                        if methods.is_empty() {
                            String::new()
                        } else {
                            format!(" · {methods}")
                        },
                        if timing.partial {
                            " · Partial context"
                        } else {
                            ""
                        }
                    )
                }),
            );
            fields.insert("timing".into(), json!(timing));
        }
        self.broadcaster.emit_context_preparation(
            event == "context.retrieval.completed",
            &self.agent_id,
            &self.principal,
            &self.workspace,
            payload,
        );
    }
}

impl Drop for ContextPreparationProgress {
    fn drop(&mut self) {
        self.finish(&ContextPreparationTiming {
            duration_ms: self.started.elapsed().as_secs_f64() * 1_000.0,
            cancelled: true,
            ..ContextPreparationTiming::default()
        });
    }
}
