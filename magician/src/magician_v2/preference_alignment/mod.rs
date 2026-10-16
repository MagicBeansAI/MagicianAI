//! Agentic preference alignment: checking a proposed act against what the
//! owner has said and what Magician has inferred about them.
//!
//! Plan: `docs/archive/plans/2026-10-07-agentic-preference-alignment-design-and-implementation-plan.md`.
//! Frozen v1 contracts: `docs/archive/plans/2026-10-09-agentic-preference-alignment-p0-inventory-and-contracts.md`.
//!
//! # Who decides what
//!
//! The judge (a model) decides whether an intention applies to this act and
//! this person, whether the conflict is material, and how to intervene. The
//! host decides everything a model must not: identity, evidence integrity,
//! which evidence may carry which force, valid transitions, and admission.
//! There are no domain rules here — no "vegetarian plus chicken means ask".
//!
//! # Two paths, both used
//!
//! An explicit owner statement (usually a preference the owner asked chat to
//! remember) can hold an act for confirmation. An inferred preference — most of
//! what Magician knows — takes part in every check and can nudge or offer a
//! better-fitting alternative, but never holds an act on its own. The host
//! enforces that split in [`contract::validate_decision`], whatever the model
//! returns.
//!
//! Admission is wired behind `preference_alignment.mode` (default `off`, which
//! changes nothing): the native loop's gate and pre-fire fence, and the
//! backstop in `execute_action_inner`. See
//! `docs/components/magician/preference-alignment.md`.

// The intention and decision contracts, the host validator and the composer
// for typed decision models live in `magician-alignment`, so they build and
// replay without this crate; host paths stay the same.
pub use magician_alignment::{composer, contract, intention};

pub mod actor;
pub mod admission;
pub mod ask;
pub mod backstop;
pub mod classify;
pub mod collect;
pub mod engine_judge;
pub mod eval_case;
pub mod generation;
pub mod history;
pub mod hold;
pub mod inventory;
pub mod judge;
pub mod lasting_write;
pub mod notices;
pub mod owner_edit;
pub mod ledger;
pub mod owner_turn;
pub mod policy;
pub mod receipts;
pub mod redact;
pub mod service;
pub mod sources;
pub mod view;
pub mod voice;
pub mod wording_grant;

#[cfg(test)]
mod journey_tests;
#[cfg(test)]
mod live_eval;
