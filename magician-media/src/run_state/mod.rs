//! Durable multi-session runs with out-of-band verification — composable work
//! modules, Module C's primitive.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §5.
//!
//! §5's case: durability for *"a multi-session authenticated form that spans
//! days and requires an out-of-band verification step"*. Three pieces:
//!
//! - **Resumable state** — *"what is filled, what is pending, what changed
//!   since. A form abandoned halfway is worse than one never started."* Run
//!   ids are derived from `(scope, purpose, resource_ref)`, so a crashed
//!   session that opens again resumes THE run; fields carry answers, pending
//!   is `answer: None`, and changed-since is a revision watermark.
//! - **Inbox-coupled verification** — *"the agent must read its own inbox
//!   mid-flow, extract the code, and continue. This is the primitive nobody
//!   has."* Here as an [`Expectation`]: the run records that it is waiting
//!   and where the event will arrive, and holds in `awaiting_external` until
//!   the caller feeds the extracted event back in.
//! - **A submit gate** — *"Submission is a submission-class act: covered only
//!   by a reviewed batch, never a standing envelope. The agent drafts the
//!   whole thing; a person presses send."* The only path to `submitted`
//!   carries a named person and the reviewed act's ref.
//!
//! The grounding rules bind every answer: *"every emitted answer cites
//! retrievable evidence — never answer from nothing"*, and *"a question the
//! evidence store cannot ground becomes an owner-facing gap, never
//! synthesised text. The owner's reply is new evidence."*
//!
//! # What this module refuses to do, on purpose
//!
//! - **It never reads an inbox** — or a phone, a calendar, or any other
//!   store. Waits are fulfilled by the caller handing in the event's ref.
//!   That decoupling is what makes the primitive serve any flow, whatever
//!   channel its verification arrives on.
//! - **It never submits.** There is no code path to `submitted` that does not
//!   carry a named person and a reviewed act — an unnamed submission is
//!   exactly how an agent would fire the gate itself.
//! - **It never stores an ungrounded answer.** An answer with no evidence
//!   refs is refused at the write and skipped in the fold; the caller records
//!   a gap and asks the owner instead.
//! - **It never invents an open-ended audience.** A run may bind a named
//!   [`AudienceRef`](magician::magician_v2::audience::AudienceRef); there is no
//!   public or wildcard binding, here or anywhere else in this codebase.

pub mod consumer;
pub mod store;
pub mod types;

#[cfg(test)]
mod tests;

pub use consumer::{
    expectations_awaiting, fulfil_from_inbound, gaps_for_owner, AwaitingExpectation, InboundEvent,
    InboundOutcome, OwnerGap,
};
pub use store::{FormSubmissionAct, RunScope, RunStateStore};
pub use types::{
    Answer, Expectation, ExpectationFulfilment, FieldState, Gap, GapResolution, Run, RunState,
    Submission,
};
