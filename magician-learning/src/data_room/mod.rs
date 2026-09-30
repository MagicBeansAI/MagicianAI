//! A data room — OPC deal close.
//!
//! Plan: `docs/plans/2026-08-07-opc-deal-close.md`.
//! Doc: `docs/components/magician/data-room.md`.
//!
//! **Written: the container, the grant, the audit log, and what is derived from
//! it.** [`store`] opens a room for an audience, adds and withdraws references
//! and closes it; [`disclosure_bridge`] records every `(document, holder)`
//! grant into the outward-assertions register; [`access_store`] persists
//! presentations and [`access_log`] derives attention from them; [`follow_ups`]
//! and [`sweep`] turn that attention into obligations and settle them again;
//! [`cycle`] carries the reading an obligation's text cannot. The audit log
//! landed before the grant deliberately: a room that could be opened before
//! anything could account for it is the disclosure the plan warns about.
//!
//! **The room never issues the link it is granted against.** Every grant takes
//! the live holders as a parameter ([`GrantDisclosure`]), and every access event
//! carries a `token_issued_to` the caller supplies. Nothing here mints a token,
//! expires one, revokes one, or serves a byte to a reader.
//!
//! # A channel, not a subsystem
//!
//! §3: *"a data room is the document channel of an engagement"*, exactly as
//! email, WhatsApp and meetings are its other channels. That one decision
//! inherits who may read it, revocation, expiry and the audit target — so this
//! module holds **no roster of its own**. What it does hold is
//! [`DocumentVisibility::permits`], and that is not a second model: it checks
//! [`Audience::admits`](magician::magician_v2::audience::Audience::admits) first
//! for both of its variants, so a per-document list can only narrow the
//! audience the caller supplied and can never outlive it.
//!
//! # References, never copies
//!
//! §4. A room holds `artifact_ref`s. Copying a document into a room would create
//! a second version that diverges from the one the owner edits — and the whole
//! point of a room is that what a counterparty sees is what we have.
//!
//! # Where the outside actually gets in
//!
//! **Not here.** This module has no `share` verb, mints no link, issues and
//! revokes no token, and serves no byte to a reader. Those live one crate over,
//! in `magician_api::data_room_reader_api` — the reader surface that takes a
//! capability URL, presents its secret against the living audience through
//! `share_links`, writes the access event **before** anything is served, and
//! then lists or serves what [`DataRoom::visible_to`] admits for that identity.
//! `magician-bin` mounts its routes, so that path is live.
//!
//! It builds this module's stores itself — [`DataRoomStore`] and
//! [`AccessStore`](access_store::AccessStore) — and resolves the roster through
//! the counterparty register rather than through anything here, which is the
//! *"no roster of its own"* rule above holding at the one place it matters.
//!
//! # Where the owner gets in
//!
//! `magician_api::data_room_api`, mounted inside `/api/magician/v2` — the grant
//! path. It opens rooms ([`DataRoomStore::open`](store::DataRoomStore::open)),
//! lists them ([`list`](store::DataRoomStore::list),
//! [`for_audience`](store::DataRoomStore::for_audience)), adds and withdraws
//! references, and issues, rotates and revokes the capability links the reader
//! presents. It is the one caller that constructs a [`GrantDisclosure`], and the
//! one that calls [`record_room_disclosures`] directly — before minting a
//! credential, because every document already in the room becomes visible to the
//! new holder the instant the link exists. A recording that fails leaves no
//! grant at all, in both directions.
//!
//! It resolves the roster through the reader surface's own `AudienceSource`
//! rather than a second copy, and takes the audience **kind** as a parameter, so
//! a panel, an account or a person is opened by the same route as an engagement.
//!
//! **Who calls the derivations, and from where.** The follow-up sweep
//! ([`sweep_follow_ups`], [`apply_follow_up_sweep`]) is run by
//! [`magician::magician_v2::obligation_sweeps::worker`], which reads a scope's
//! rooms, grants and access lane and writes the obligations they imply. The
//! attention derivations are also read cross-room by
//! [`magician::magician_v2::outcome_learning::composition::market_read`] for the
//! market read; its feeders still take [`AccessEvent`]s as parameters and load
//! nothing themselves, which is what keeps them usable by any flow that
//! observes attention some other way.

pub mod access_log;
// The persistence lane behind the audit log: without it an AccessEvent has a
// type and derivations but nowhere to live, so every derivation runs over data
// that cannot exist.
pub mod access_store;
// §6's derived states in the words the owning agent's cycle reads. The register
// carries what is OWED; this carries what the counterparty actually did, keyed
// to the register row it explains.
pub mod cycle;
pub mod disclosure_bridge;
pub mod follow_ups;
pub mod store;
pub mod sweep;
pub mod types;

#[cfg(test)]
mod tests;

pub use access_log::{
    attention_across, attention_for, document_reach, AccessEvent, AttentionSignal, TokenAttention,
    UserAgentClass, ROOM_LOGGING_NOTICE,
};
pub use cycle::{actionable, attention_notes, AttentionNote, AttentionReading, SharedRoom};
pub use disclosure_bridge::record_room_disclosures;
pub use follow_ups::{derive_follow_ups, next_review_at, FollowUpPolicy, TokenContext};
pub use store::{DataRoomScope, DataRoomStore, GrantDisclosure};
pub use sweep::{
    apply_follow_up_sweep, snapshot_from_events, sweep_follow_ups, AppliedFollowUpSweep,
    FollowUpSweep, IdentifiedObligation, SharedToken, FOLLOW_UP_RELEASE_REASON,
};
pub use types::{
    names_a_revision, split_document_ref, DataRoom, DocumentEntry, DocumentVisibility,
    OpenDataRoom, RoomStanding, REVISION_SEP,
};
