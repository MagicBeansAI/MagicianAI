//! What is owed, and by when — composable work modules, Module D.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §6.
//! Doc: `docs/components/magician/obligations.md`.
//!
//! §6 in full: *"Small, and the review found nothing owns it. Program running
//! state tracks stage, not obligation. If a counterparty says 'send me your
//! metrics by Friday', nothing enforces Friday."*
//!
//! The plan writes the first term of the contract tuple as *engagement*; it is
//! an [`AudienceRef`](crate::magician_v2::audience::AudienceRef), of which an
//! engagement is one kind. Promises are made to clients, cohorts, panels and
//! individuals as readily as to counterparties.
//!
//! # Generic
//!
//! *"Reusable everywhere. Nothing about it is fundraising-specific."* An
//! obligation is a promise with a deadline and a direction — the same shape for
//! a deck owed to an investor, a reply owed to a customer, or a document a
//! supplier owes us.
//!
//! # The two things it does that a to-do list does not
//!
//! - **Direction.** A lapse means two different things. One list that read alike
//!   for "we are late" and "they have not replied" would train the owner to skim
//!   it.
//! - **Derived lapsing.** Nothing writes `lapsed`. It falls out of the clock, so
//!   the register cannot depend on a sweep having run — and catching what nobody
//!   remembered is the entire job.

pub mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use store::{ObligationScope, ObligationStore};
pub use types::{Obligation, ObligationDirection, ObligationState, RecordObligation, Settlement};
