//! System agent wrappers for the planning pipeline.
//!
//! Each wrapper holds an `Arc<ExistingService>`, reads input artifacts from
//! the store, calls the underlying service unchanged, and writes output
//! artifacts back.

pub mod elicitor;
pub mod slot_extractor;

pub mod answer_interpreter;
pub mod planner;
pub mod query_rewriter;

pub use answer_interpreter::AnswerInterpreterAgent;
pub use elicitor::ElicitorAgent;
pub use planner::{PlannerAgent, PlannerBackend};
pub use query_rewriter::QueryRewriterAgent;
pub use slot_extractor::SlotExtractorAgent;
pub mod scheduler;
pub use scheduler::SchedulerAgent;
pub mod intent_classifier;
pub use intent_classifier::IntentClassifierAgent;
pub mod autonomous_executor;
pub mod plan_patcher;
pub use plan_patcher::PlanPatcherAgent;
