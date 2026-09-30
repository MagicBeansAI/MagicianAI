//! Vendor-neutral structured-decision plane.
//!
//! One seam, three layers: deterministic code owns policy, this crate owns
//! typed decisions (Choice / Score / Noul) and their model adapters, and
//! generative LLMs stay where prose is the product. The crate deliberately
//! depends on neither `magician` nor `magician-comms` nor any decision-model
//! vendor, so a later Jev-style successor is a new adapter, not a rewrite of
//! the consumers (the plan of record is
//! `docs/plans/2026-09-19-structured-decision-plane-plan.md`).
//!
//! Locked rules carried from that plan:
//!
//! - Structured decision is **not** an LLM profile. It never rides a
//!   `complete() -> String` seam; answers keep their probability
//!   distributions and confidence, because calibration is what lets code
//!   act autonomously on a decision.
//! - Answers are constrained to the supplied options. An adapter that
//!   returns an unknown option is a [`DecisionError::UnknownOption`]
//!   transport error, never a silent coerce to a default label.
//! - Noul carries no separate confidence. Distance from 0.5 is the
//!   uncertainty signal, so a Choice-tuned threshold must not be carried
//!   onto a Noul question.
//! - Unbound means off. A configured-but-unbound operation never falls back
//!   to an LLM router default; the caller keeps its incumbent path.

pub mod action;
pub mod adapters;
pub mod admission;
pub mod compose;
pub mod config;
mod dispatch;
pub mod engine;
pub mod error;
pub mod host;
pub mod model;
pub mod pack;
pub mod primitives;
pub mod registry;
pub mod request;
pub mod runtime;
pub mod telemetry;

pub use compose::ShadowComparison;
pub use error::DecisionError;
pub use model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
pub use pack::{Pack, PackStore};
pub use primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulCriteria, NoulQuestion, OptionId, PrimitiveKind,
    Question, QuestionId, ScoreQuestion,
};
pub use request::{
    set_choice_candidates, Answer, DecisionRequest, DecisionResponse, DecisionState, Usage,
};
pub use runtime::{BoundModel, BoundOperation, DecisionRuntime, DecisionRuntimeBuilder};
