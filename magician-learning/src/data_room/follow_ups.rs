//! Attention signals become obligations — deal-close plan phase 4, §6 "How it
//! surfaces".
//!
//! Plan: `docs/plans/2026-08-07-opc-deal-close.md`.
//!
//! §6: *"Into obligations (Composable Work Modules, Module D), which already
//! owns 'what is owed and by when'. Opened-then-silent becomes a follow-up with
//! a known basis; never-opened becomes a delivery question rather than a nudge.
//! Today both are indistinguishable from a quiet inbox."*
//!
//! # No store of its own — the obligation register is the store
//!
//! Deliberate absence. This module is pure functions that phrase what a room's
//! attention signals imply as [`RecordObligation`]s; recording them, settling
//! them, and collapsing the same item noticed twice into one entry are the
//! register's job (Module D). A second ledger here would be the thing that
//! disagrees with it.
//!
//! # The reply lives outside the room
//!
//! [`AttentionSignal`] deliberately has no *"opened once, then silence"* state,
//! because whether they replied is a fact the room does not have — replies
//! arrive by mail, by phone, in meetings. This module is the caller-side join
//! the access log points at: [`TokenContext::replied`] is **supplied** by
//! whoever holds that knowledge. Nothing here reads an inbox, a calendar, an
//! engagement store, or configuration — every input arrives as a parameter,
//! which is what lets any flow use it.
//!
//! # `due_at` is the ripening instant, never the sweep clock
//!
//! The obligation register derives its ids from `(audience, what, due_at,
//! direction)`, so an obligation is idempotent exactly when that tuple is
//! stable across sweeps. A `due_at` of "now" changes on every sweep and would
//! mint a fresh obligation per run — the register-flooding failure that makes
//! people ignore a to-do list. So `due_at` is the instant the item **ripened**
//! (`shared_at + window` for a delivery question, `last_seen + window` for a
//! follow-up): already actionable the moment it is produced, and identical on
//! every later sweep. For the same reason the `what` strings carry no
//! timestamps — a timestamp in the text would smuggle the sweep clock back into
//! the identity tuple.
//!
//! ## Born ripe means sometimes born already lapsed — deliberately
//!
//! A consequence worth stating plainly: [`derive_follow_ups`] returns only
//! ripened items and `due_at` is the ripening instant, so every obligation it
//! emits is already at-or-past due the moment it is recorded — a sweep that
//! runs long after ripening records an item that enters the register overdue
//! by the sweep lag, skipping the open state entirely. This is deliberate,
//! twice over. Stability: a grace or clamp that nudged `due_at` toward the
//! sweep clock would change the identity tuple between sweeps and mint
//! duplicates — the exact flooding failure this design exists to prevent.
//! Honesty: "already actionable" is the true state of a ripened follow-up; the
//! waiting window WAS the grace, it has elapsed, and the register must not
//! restart the clock just because we were slow to look. A surfacing layer that
//! wants to tell "we just noticed" apart from "we sat on this" may compare
//! when the row was recorded against its `due_at` — that distinction is
//! presentation, and it must never leak into the identity tuple.
//!
//! # The room's stable id is the identity carrier, never its display label
//!
//! The `what` text is part of the register's identity tuple, so everything in
//! it must be as stable as the fact it names. A room's display label is
//! mutable — were it in the text, renaming the room would change the derived
//! id and mint a duplicate of an obligation the register already holds, while
//! the original could never be collapsed or re-derived again; and two distinct
//! rooms sharing a label would collapse genuinely different follow-ups into
//! one indistinguishable row. So the functions here take the room's **stable
//! id** and put that in the text. The recorded text is register identity, not
//! prose for a screen: a display layer may decorate what it surfaces with the
//! current human label, but the label must never reach what is recorded.
//!
//! # A delivery question is our work item, not a nudge
//!
//! §6: never-opened *"becomes a delivery question rather than a nudge"*. The
//! obligation raised for a never-opened token is `OwedByUs` and reads as
//! **our** task — verify the link arrived, try another channel. It must never
//! be phrased as chasing the recipient: nudging someone who never received the
//! mail is the wrong act and reads as pestering
//! (see [`AttentionSignal::is_delivery_question`]).

use std::collections::HashSet;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::obligations::{ObligationDirection, RecordObligation};

use super::access_log::{AttentionSignal, TokenAttention};

/// The `created_by` every obligation from this module carries.
///
/// Names the mechanism, so a register row is traceable to the sweep that raised
/// it. Not part of the register's identity tuple, but it must still be stable:
/// a value that varied would make otherwise-identical rows read as if they had
/// different origins.
pub const FOLLOW_UPS_CREATED_BY: &str = "data-room-follow-ups";

/// The waiting windows.
///
/// Fields are private on purpose: the only way to obtain a policy is
/// [`FollowUpPolicy::new`], which refuses non-positive windows, so a zero
/// window cannot exist anywhere downstream — the refusal is structural, not a
/// convention a struct literal could bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpPolicy {
    /// How long a shared, never-opened token may sit before it becomes a
    /// delivery question.
    delivery_question_after: Duration,
    /// How long an opened-but-unreplied token may sit after its **last** visit
    /// before it becomes a follow-up.
    follow_up_after: Duration,
}

impl FollowUpPolicy {
    /// Build a policy, refusing non-positive windows.
    ///
    /// A zero (or negative) window raises an item the instant a room is shared
    /// — every share would flood the register with a delivery question before
    /// the mail has had any chance to arrive, and every open would become an
    /// immediate nudge. The same bug class as a zero maturity window: the wait
    /// IS the meaning, so a policy without one is refused rather than defaulted.
    pub fn new(delivery_question_after: Duration, follow_up_after: Duration) -> Result<Self> {
        if delivery_question_after <= Duration::zero() {
            anyhow::bail!(
                "the delivery-question window must be positive: a non-positive window raises a \
                 delivery question the instant a room is shared, flooding the register before \
                 the mail has had any chance to arrive"
            );
        }
        if follow_up_after <= Duration::zero() {
            anyhow::bail!(
                "the follow-up window must be positive: a non-positive window turns every open \
                 into an instant follow-up, which is the nudge-on-sight behaviour this module \
                 exists to prevent"
            );
        }
        Ok(Self {
            delivery_question_after,
            follow_up_after,
        })
    }

    pub fn delivery_question_after(&self) -> Duration {
        self.delivery_question_after
    }

    pub fn follow_up_after(&self) -> Duration {
        self.follow_up_after
    }
}

/// One token's situation, as the caller knows it.
///
/// The room contributes [`TokenContext::attention`]; everything else is
/// knowledge the room does not have and must be supplied by whoever does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenContext {
    /// What the room observed for this token — from
    /// [`attention_for`](super::access_log::attention_for) or
    /// [`attention_across`](super::access_log::attention_across).
    pub attention: TokenAttention,
    /// When the room was shared with this token. The never-opened window runs
    /// from the share, not from any event, because a never-opened token has no
    /// events to run from.
    pub shared_at: DateTime<Utc>,
    /// Whether they have replied — on **any** channel. The room cannot know
    /// this; the caller who holds the correspondence supplies it.
    ///
    /// `true` suppresses everything, the delivery question included: a
    /// never-opened token cannot have replied *through the room*, but replies
    /// arrive elsewhere, and a reply proves the message landed — delivery is
    /// moot, and any generated item would be noise on a live conversation.
    pub replied: bool,
}

/// The ripe obligations implied by a room's attention signals.
///
/// **Paired with [`follow_ups_to_settle`] — recording is half the life-cycle.**
/// A sweep that records this function's output must also diff the previous
/// snapshot against the current one with [`follow_ups_to_settle`] and settle
/// what it returns. An advancing basis mints a NEW tuple (a later `last_seen`
/// is a later `due_at`, hence a new derived id in the register) while the
/// superseded row stays open, and a reply suppresses only *future* derivation,
/// never a row already recorded. Deriving without the paired settle leaks one
/// open obligation per visit-then-silence cycle, forever.
///
/// Only items whose window has closed at `now` are returned — the window is
/// **inclusive**, like every deadline in this codebase: ripening at noon means
/// ripe at noon. For when the next item will ripen, use [`next_review_at`];
/// returning unripe items here would hand the caller obligations it must not
/// record yet, which is how "someday" items leak into a register.
///
/// Guarantees:
/// - **Stable output per token.** `due_at` is the ripening instant and the
///   `what` text carries no timestamps, so repeated sweeps after ripening
///   produce the identical `(audience, what, due_at, direction)` tuple and the
///   register's derived id collapses them into one obligation. See the module
///   note — this is the property the whole design hangs on.
/// - **One item per token.** A duplicated token in the input contributes once,
///   first occurrence wins. Two contexts for one token with different last-seen
///   instants would otherwise yield two due dates — two derived ids in the
///   register — and the same item would surface twice.
/// - **Deterministic order.** Soonest-due first, then by text — the order the
///   register itself surfaces, so a sweep's output reads the way the register
///   will.
///
/// An empty `tokens` slice yields an empty list. That is "no one to consider",
/// not a claim that anyone is fine — the vacuous case must not be read as a
/// healthy room.
///
/// Every obligation is `OwedByUs`: both the delivery check and the follow-up
/// are **our** next action. `program_id` and `source_act_ref` are `None`
/// because the room knows neither — it does not know which programme a share
/// belonged to, and it never saw the act that made the share; fabricating
/// either would misfile the item or link it to something that did not happen.
///
/// `room_id` is the room's **stable identifier**, never its display label. It
/// feeds the `what` text, which is part of the register's identity tuple — a
/// mutable label there would let a rename mint duplicate obligations and let
/// two rooms sharing a label collapse into one. See the module note: display
/// decoration happens at the surface, never in the recorded text.
pub fn derive_follow_ups(
    audience: &AudienceRef,
    room_id: &str,
    tokens: &[TokenContext],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Vec<RecordObligation> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for token in tokens {
        // First occurrence wins, so derive and next_review_at agree on which
        // context speaks for a duplicated token.
        if !seen.insert(token.attention.token_issued_to.as_str()) {
            continue;
        }
        let Some(ripens_at) = ripening_instant(token, policy) else {
            continue;
        };
        if now < ripens_at {
            // Inside the window: nothing yet. next_review_at names the instant.
            continue;
        }
        let what = match token.attention.signal() {
            AttentionSignal::NeverOpened => {
                delivery_question_what(room_id, &token.attention.token_issued_to)
            },
            AttentionSignal::OpenedOnce | AttentionSignal::OpenedRepeatedly => {
                follow_up_what(room_id, &token.attention.token_issued_to)
            },
        };
        out.push(RecordObligation {
            audience: audience.clone(),
            // The room does not know which programme a share belonged to, and
            // guessing would misfile the item.
            program_id: None,
            what,
            due_at: ripens_at,
            direction: ObligationDirection::OwedByUs,
            created_by: FOLLOW_UPS_CREATED_BY.to_string(),
            // The room never saw the act that made the share; a fabricated
            // reference would link the obligation to something that did not
            // happen.
            source_act_ref: None,
        });
    }
    out.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.what.cmp(&right.what))
    });
    out
}

/// The obligations a previous snapshot implied that the current one no longer
/// does — the settle half of the pairing on [`derive_follow_ups`].
///
/// A pure diff, like everything in this module: the tuples that were derivable
/// from `previous` (ripened under `policy` at `now`) but are **not** derivable
/// from `current` — the token replied, or its basis advanced to a later
/// `last_seen`/`shared_at` so the current sweep derives a newer tuple for the
/// same token. Each returned value byte-matches what [`derive_follow_ups`]
/// produced for `previous`, so the caller rebuilds the register's id for each
/// with [`obligation_id_for`](magician::magician_v2::obligations::store::obligation_id_for)
/// and settles it as
/// [`Settlement::Released`](magician::magician_v2::obligations::Settlement::Released)
/// with a reason naming the new basis — `Released`, never `Met`: a superseded
/// follow-up was not done, it stopped applying.
///
/// Identity is `(what, due_at)` under the fixed audience and direction —
/// exactly the register's derivation tuple. So an advanced basis yields the
/// OLD tuple here while [`derive_follow_ups`] on `current` yields the new one
/// (same text, different `due_at`), and a tuple still derivable from `current`
/// is never returned — settling it would fight the recording half. Nothing
/// ripened from `previous` yields nothing to settle: the recording half never
/// produced those rows, and this function must not invent settlements for
/// rows that were never recordable.
pub fn follow_ups_to_settle(
    audience: &AudienceRef,
    room_id: &str,
    previous: &[TokenContext],
    current: &[TokenContext],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Vec<RecordObligation> {
    let still_derivable: HashSet<(String, DateTime<Utc>)> =
        derive_follow_ups(audience, room_id, current, policy, now)
            .into_iter()
            .map(|obligation| (obligation.what, obligation.due_at))
            .collect();
    derive_follow_ups(audience, room_id, previous, policy, now)
        .into_iter()
        .filter(|obligation| {
            !still_derivable.contains(&(obligation.what.clone(), obligation.due_at))
        })
        .collect()
}

/// The earliest instant anything in `tokens` could ripen **after** `now`, so a
/// cycle knows when to come back without polling blindly.
///
/// Strictly after, on purpose: this function answers "when could something NEW
/// ripen", and an instant at or before `now` answers nothing — everything ripe
/// at `now` (the window is inclusive) is already fully carried in
/// [`derive_follow_ups`]'s output for the same sweep. An earlier cut took the
/// minimum over ALL ripening instants, and one already-ripened token then
/// masked every future ripening forever: the caller was handed the same stale
/// past instant on every call and could never learn when the next unripe token
/// would ripen.
///
/// `None` means nothing here will ripen after `now` on these facts — every
/// token replied, already ripened (and so is in the sweep's output), or is
/// contradictory input. It is not a claim that the room needs no attention: a
/// new share or a new visit changes the facts and the answer with them.
///
/// Tokens are deduplicated first-occurrence-wins, exactly as
/// [`derive_follow_ups`] does, so what a sweep raises and when a cycle is told
/// to wake can never disagree.
pub fn next_review_at(
    tokens: &[TokenContext],
    policy: &FollowUpPolicy,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let mut seen: HashSet<&str> = HashSet::new();
    tokens
        .iter()
        .filter(|token| seen.insert(token.attention.token_issued_to.as_str()))
        .filter_map(|token| ripening_instant(token, policy))
        .filter(|ripens_at| *ripens_at > now)
        .min()
}

/// The instant one token's situation becomes actionable, or `None` if it never
/// will on the facts supplied.
///
/// One function on purpose: [`derive_follow_ups`] and [`next_review_at`] both
/// call it, so what a sweep raises and when a cycle is told to come back cannot
/// drift apart.
///
/// `None` is fail-closed, never permissive:
/// - `replied` suppresses everything, the delivery question included — see
///   [`TokenContext::replied`].
/// - an opened signal with no `last_seen` is contradictory input: a visit with
///   no time. There is no honest ripening instant to derive from it, and
///   substituting one (the share time, the sweep clock) would put a wrong or
///   moving `due_at` into the register — so it yields nothing rather than
///   something wrong.
fn ripening_instant(token: &TokenContext, policy: &FollowUpPolicy) -> Option<DateTime<Utc>> {
    if token.replied {
        return None;
    }
    match token.attention.signal() {
        AttentionSignal::NeverOpened => Some(token.shared_at + policy.delivery_question_after),
        AttentionSignal::OpenedOnce | AttentionSignal::OpenedRepeatedly => token
            .attention
            .last_seen
            .map(|last_seen| last_seen + policy.follow_up_after),
    }
}

/// The delivery-question wording — our task, aimed at us.
///
/// §6: never-opened *"becomes a delivery question rather than a nudge"*. The
/// text names the token and tells the owner to check the channel, never to
/// chase the recipient: nudging someone who never received the mail is the
/// wrong act. Deterministic — no timestamps, and the room's stable id rather
/// than its mutable display label — because the text is part of the register's
/// identity tuple; a display layer may decorate what it surfaces with the
/// human label, but the recorded text never carries it.
fn delivery_question_what(room_id: &str, token_issued_to: &str) -> String {
    format!(
        "verify delivery of data room '{room_id}' to {token_issued_to}: never opened; \
         confirm the link arrived or try another channel"
    )
}

/// The follow-up wording — the token, and the basis.
///
/// §6: *"Opened-then-silent becomes a follow-up with a known basis."* The basis
/// is in the text so the owner acts on evidence rather than a bare name.
/// Deterministic — no timestamps, the stable room id, never the display
/// label — for the same identity-tuple reason as the delivery question.
fn follow_up_what(room_id: &str, token_issued_to: &str) -> String {
    format!("follow up with {token_issued_to} on data room '{room_id}': opened, no reply")
}

#[cfg(test)]
mod tests {
    //! §6's "How it surfaces" contract, as behaviour.

    use chrono::{DateTime, Duration, TimeZone, Utc};

    use crate::data_room::access_log::TokenAttention;
    use magician::magician_v2::audience::AudienceRef;
    use magician::magician_v2::obligations::ObligationDirection;

    use super::{
        derive_follow_ups, follow_ups_to_settle, next_review_at, FollowUpPolicy, TokenContext,
        FOLLOW_UPS_CREATED_BY,
    };

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn fixture() -> (AudienceRef, FollowUpPolicy) {
        (
            AudienceRef::engagement("eng-1"),
            FollowUpPolicy::new(Duration::days(3), Duration::days(5)).expect("positive windows"),
        )
    }

    fn token(
        issued_to: &str,
        visits: u32,
        shared_at: DateTime<Utc>,
        last_seen: Option<DateTime<Utc>>,
        replied: bool,
    ) -> TokenContext {
        TokenContext {
            attention: TokenAttention {
                token_issued_to: issued_to.to_string(),
                visits,
                presentations: visits as usize,
                first_seen: last_seen,
                last_seen,
                documents_opened: Vec::new(),
                documents_unopened: Vec::new(),
                index_views: 0,
            },
            shared_at,
            replied,
        }
    }

    /// §6: never-opened *"becomes a delivery question rather than a nudge"*.
    /// The window is inclusive: `shared_at + window` IS the ripening instant,
    /// and one second earlier is still inside the window.
    #[test]
    fn never_opened_ripens_into_a_delivery_question_at_the_window_inclusive() {
        let (audience, policy) = fixture();
        let shared_at = now() - Duration::days(3);
        let tokens = vec![token("alice", 0, shared_at, None, false)];

        assert!(
            derive_follow_ups(
                &audience,
                "room-7c2f",
                &tokens,
                &policy,
                now() - Duration::seconds(1)
            )
            .is_empty(),
            "one second short of the window is still inside it"
        );

        let ripe = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(ripe.len(), 1);
        let obligation = &ripe[0];
        assert_eq!(obligation.audience, audience);
        assert_eq!(obligation.program_id, None);
        assert_eq!(
            obligation.what,
            "verify delivery of data room 'room-7c2f' to alice: never opened; confirm the link \
             arrived or try another channel"
        );
        assert_eq!(obligation.due_at, shared_at + Duration::days(3));
        assert_eq!(obligation.direction, ObligationDirection::OwedByUs);
        assert_eq!(obligation.created_by, FOLLOW_UPS_CREATED_BY);
        assert_eq!(obligation.source_act_ref, None);
    }

    /// `due_at` is the ripening instant, not the sweep clock: a sweep an hour
    /// later produces the SAME `(audience, what, due_at, direction)` tuple, so
    /// the register's derived id collapses both sweeps into one obligation. A
    /// due date that moved with the sweep would mint a new obligation per run.
    /// The due date sits in the past at both sweeps — born already actionable,
    /// and when swept late, born already lapsed: the stable, honest state (see
    /// the module note), not a defect to grace away.
    #[test]
    fn a_later_sweep_produces_the_same_obligation_tuple() {
        let (audience, policy) = fixture();
        let shared_at = now() - Duration::days(4);
        let tokens = vec![token("alice", 0, shared_at, None, false)];

        let first = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        let later = derive_follow_ups(
            &audience,
            "room-7c2f",
            &tokens,
            &policy,
            now() + Duration::hours(1),
        );
        assert_eq!(first.len(), 1);
        assert_eq!(later.len(), 1);
        assert_eq!(first[0].audience, later[0].audience);
        assert_eq!(first[0].what, later[0].what);
        assert_eq!(first[0].due_at, later[0].due_at);
        assert_eq!(first[0].direction, later[0].direction);
        assert_eq!(
            first[0].due_at,
            shared_at + Duration::days(3),
            "the instant it ripened, not either sweep's clock"
        );
    }

    /// Pins the mutable-label identity failure: the `what` text once carried
    /// the room's display label, so renaming a room changed the register's
    /// identity tuple — the next sweep minted a duplicate obligation while
    /// the original lingered uncollapsible — and two distinct rooms sharing a
    /// label collapsed into one indistinguishable row. The identity carrier
    /// is the room's stable id: a rename cannot touch the tuple because the
    /// label is no longer an input at all, and distinct rooms stay distinct
    /// however they are labelled.
    #[test]
    fn room_identity_is_the_stable_id_so_a_rename_cannot_mint_a_duplicate() {
        let (audience, policy) = fixture();
        let shared_at = now() - Duration::days(4);
        let tokens = vec![token("alice", 0, shared_at, None, false)];

        // Two sweeps of the same room; the user renamed it in between. The
        // display label is not an input, so the tuples are identical and the
        // register's derived id collapses both into one obligation.
        let before_rename = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        let after_rename = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(before_rename.len(), 1);
        assert_eq!(after_rename.len(), 1);
        assert_eq!(
            before_rename[0].what,
            "verify delivery of data room 'room-7c2f' to alice: never opened; confirm the \
             link arrived or try another channel"
        );
        assert_eq!(before_rename[0].what, after_rename[0].what);
        assert_eq!(before_rename[0].due_at, after_rename[0].due_at);
        assert_eq!(before_rename[0].audience, after_rename[0].audience);
        assert_eq!(before_rename[0].direction, after_rename[0].direction);

        // A different room shared with the same audience and token: the ids
        // differ, so the tuples differ, whatever either room is labelled.
        let other_room = derive_follow_ups(&audience, "room-91aa", &tokens, &policy, now());
        assert_eq!(
            other_room[0].what,
            "verify delivery of data room 'room-91aa' to alice: never opened; confirm the \
             link arrived or try another channel"
        );
        assert_ne!(before_rename[0].what, other_room[0].what);
    }

    /// §6: *"Opened-then-silent becomes a follow-up with a known basis."* The
    /// silence window runs from the LAST visit, not from the share — a token
    /// that came back recently is not silent, however old the share is.
    #[test]
    fn opened_no_reply_ripens_off_last_seen_not_shared_at() {
        let (audience, policy) = fixture();
        let shared_at = now() - Duration::days(30);
        let last_seen = now() - Duration::days(5);
        let tokens = vec![token("bob", 1, shared_at, Some(last_seen), false)];

        assert!(
            derive_follow_ups(
                &audience,
                "room-7c2f",
                &tokens,
                &policy,
                now() - Duration::seconds(1)
            )
            .is_empty(),
            "the share being a month old does not make the silence older"
        );

        let ripe = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(ripe.len(), 1);
        assert_eq!(
            ripe[0].what,
            "follow up with bob on data room 'room-7c2f': opened, no reply"
        );
        assert_eq!(
            ripe[0].due_at,
            last_seen + Duration::days(5),
            "ripens off last_seen; shared_at plays no part once they opened"
        );
        assert_eq!(ripe[0].direction, ObligationDirection::OwedByUs);
        assert_eq!(ripe[0].source_act_ref, None);
    }

    /// Repeat visits change the conversation, not the obligation shape: opened
    /// repeatedly then silent is still opened-then-silent once the visits stop.
    #[test]
    fn opened_repeatedly_without_reply_ripens_the_same_follow_up() {
        let (audience, policy) = fixture();
        let last_seen = now() - Duration::days(6);
        let tokens = vec![token(
            "carol",
            4,
            now() - Duration::days(10),
            Some(last_seen),
            false,
        )];

        let ripe = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(ripe.len(), 1);
        assert_eq!(
            ripe[0].what,
            "follow up with carol on data room 'room-7c2f': opened, no reply"
        );
        assert_eq!(ripe[0].due_at, last_seen + Duration::days(5));
    }

    /// `replied == true` produces NOTHING even when long overdue: the
    /// conversation is alive, and a follow-up would be noise on it.
    #[test]
    fn a_reply_suppresses_the_follow_up_even_when_long_overdue() {
        let (audience, policy) = fixture();
        let tokens = vec![token(
            "dan",
            1,
            now() - Duration::days(60),
            Some(now() - Duration::days(50)),
            true,
        )];

        assert!(derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now()).is_empty());
        assert_eq!(
            next_review_at(&tokens, &policy, now()),
            None,
            "a replied token will never ripen"
        );
    }

    /// A token both never-opened and replied is contradictory THROUGH THE ROOM
    /// — nobody replies via a link they never presented — but replies arrive on
    /// other channels. They answered elsewhere, so delivery is moot and even
    /// the delivery question is suppressed.
    #[test]
    fn a_reply_on_another_channel_suppresses_even_the_delivery_question() {
        let (audience, policy) = fixture();
        let tokens = vec![token("erin", 0, now() - Duration::days(60), None, true)];

        assert!(derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now()).is_empty());
        assert_eq!(next_review_at(&tokens, &policy, now()), None);
    }

    /// Inside the window nothing ripens, and next_review_at names the exact
    /// instant it will — so a cycle knows when to come back without polling
    /// blindly.
    #[test]
    fn inside_the_window_nothing_ripens_and_next_review_names_the_instant() {
        let (audience, policy) = fixture();
        let shared_at = now() - Duration::days(1);
        let tokens = vec![token("alice", 0, shared_at, None, false)];

        assert!(derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now()).is_empty());
        assert_eq!(
            next_review_at(&tokens, &policy, now()),
            Some(shared_at + Duration::days(3))
        );
    }

    /// next_review_at is the EARLIEST ripening instant across the tokens — a
    /// cycle that woke any later could miss the first item. Replied tokens
    /// contribute nothing.
    #[test]
    fn next_review_at_is_the_earliest_instant_across_tokens() {
        let (_audience, policy) = fixture();
        let tokens = vec![
            // Never opened: ripens at shared_at + the delivery window.
            token("alice", 0, now() - Duration::days(1), None, false),
            // Opened: ripens at last_seen + the follow-up window — sooner.
            token(
                "bob",
                1,
                now() - Duration::days(10),
                Some(now() - Duration::days(4)),
                false,
            ),
            // Replied: never ripens.
            token(
                "carol",
                1,
                now() - Duration::days(10),
                Some(now() - Duration::days(9)),
                true,
            ),
        ];

        assert_eq!(
            next_review_at(&tokens, &policy, now()),
            Some(now() + Duration::days(1))
        );
    }

    /// Pins the masking failure: next_review_at once took the minimum over
    /// ALL ripening instants, so a single already-ripened token returned its
    /// stale past instant on every call, forever — and the caller could never
    /// learn when the next unripe token would ripen. A cycle scheduling its
    /// wake from that value either hot-looped on the present or fell back to
    /// blind polling, while the future ripening went unannounced.
    #[test]
    fn a_ripened_token_does_not_mask_the_next_future_ripening() {
        let (audience, policy) = fixture();
        let tokens = vec![
            // Ripened a day ago: the sweep's business, not the wake-up's.
            token("alice", 0, now() - Duration::days(4), None, false),
            // Ripens a day from now.
            token(
                "bob",
                1,
                now() - Duration::days(10),
                Some(now() - Duration::days(4)),
                false,
            ),
        ];

        assert_eq!(
            next_review_at(&tokens, &policy, now()),
            Some(now() + Duration::days(1)),
            "the future instant — the ripened token's past instant answers nothing"
        );
        // The ripened item is not lost to the strictly-after rule: the same
        // sweep already carries it, due at its own ripening instant.
        let ripe = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(ripe.len(), 1);
        assert_eq!(ripe[0].due_at, now() - Duration::days(1));

        // Every instant in the past: nothing NEW can ripen, so there is no
        // wake-up time — None, while the sweep still carries the ripe item.
        let all_past = vec![token("alice", 0, now() - Duration::days(4), None, false)];
        assert_eq!(next_review_at(&all_past, &policy, now()), None);
        assert_eq!(
            derive_follow_ups(&audience, "room-7c2f", &all_past, &policy, now()).len(),
            1
        );

        // Ripening exactly at `now` is ripe — the window is inclusive — so it
        // belongs to the sweep, not the wake-up: strictly after excludes it.
        let at_now = vec![token("carol", 0, now() - Duration::days(3), None, false)];
        assert_eq!(next_review_at(&at_now, &policy, now()), None);
        assert_eq!(
            derive_follow_ups(&audience, "room-7c2f", &at_now, &policy, now()).len(),
            1
        );
    }

    /// An empty token list yields nothing and no review instant. That is "no
    /// one to consider", not a claim that anyone is fine — the vacuous case
    /// must not read as a healthy room.
    #[test]
    fn an_empty_token_list_yields_nothing_and_no_review_instant() {
        let (audience, policy) = fixture();

        assert_eq!(
            derive_follow_ups(&audience, "room-7c2f", &[], &policy, now()).len(),
            0
        );
        assert_eq!(next_review_at(&[], &policy, now()), None);
    }

    /// A zero window raises an item the instant a room is shared — the zero
    /// maturity-window bug class. The policy refuses to exist with one, in
    /// either window, zero or negative.
    #[test]
    fn the_policy_refuses_non_positive_windows() {
        let err = FollowUpPolicy::new(Duration::zero(), Duration::days(5))
            .expect_err("zero delivery window");
        assert!(
            err.to_string()
                .contains("delivery-question window must be positive"),
            "{err}"
        );

        let err = FollowUpPolicy::new(Duration::days(3), Duration::zero())
            .expect_err("zero follow-up window");
        assert!(
            err.to_string()
                .contains("follow-up window must be positive"),
            "{err}"
        );

        assert!(FollowUpPolicy::new(Duration::days(-1), Duration::days(5)).is_err());
        assert!(FollowUpPolicy::new(Duration::days(3), Duration::days(-1)).is_err());
    }

    /// An opened signal with no last_seen is contradictory input — a visit with
    /// no time. Fail closed: no honest ripening instant exists, and a
    /// substituted one would put a wrong or moving due date into the register.
    #[test]
    fn an_opened_signal_with_no_last_seen_yields_nothing() {
        let (audience, policy) = fixture();
        let tokens = vec![token("frank", 2, now() - Duration::days(30), None, false)];

        assert!(derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now()).is_empty());
        assert_eq!(next_review_at(&tokens, &policy, now()), None);
    }

    /// A duplicated token in the input contributes ONCE, first occurrence
    /// wins — in both functions. Two contexts for one token with different
    /// last-seen instants would otherwise yield two due dates, hence two
    /// derived ids in the register, and the same item would surface twice.
    /// The duplicate here ripens SOONER than the first occurrence, so a plain
    /// minimum over both instants would betray a dedup failure in either
    /// function.
    #[test]
    fn a_duplicated_token_context_contributes_once_first_occurrence_wins() {
        let (audience, policy) = fixture();
        let first_last_seen = now() - Duration::days(6);
        let tokens = vec![
            token(
                "alice",
                1,
                now() - Duration::days(30),
                Some(first_last_seen),
                false,
            ),
            // A stale duplicate of the same token, one visit earlier — it
            // would ripen sooner, so it must not be the context that speaks.
            token(
                "alice",
                1,
                now() - Duration::days(30),
                Some(now() - Duration::days(7)),
                false,
            ),
        ];

        let ripe = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now());
        assert_eq!(ripe.len(), 1);
        assert_eq!(ripe[0].due_at, first_last_seen + Duration::days(5));

        // Asked from inside the window, so the first occurrence's instant is
        // still upcoming: the wake-up names it, not the duplicate's sooner one.
        let asked_at = now() - Duration::days(3);
        assert_eq!(
            next_review_at(&tokens, &policy, asked_at),
            Some(first_last_seen + Duration::days(5)),
            "the sweep and the wake-up must see the same world"
        );
    }

    /// Output is deterministic: soonest-due first, then by text — the same
    /// order the register itself surfaces, whatever order the caller supplied
    /// the tokens in.
    #[test]
    fn output_is_ordered_soonest_due_first_whatever_the_input_order() {
        let (audience, policy) = fixture();
        let tokens = vec![
            // Ripened just now: supplied first.
            token("alice", 0, now() - Duration::days(3), None, false),
            // Ripened two days ago.
            token(
                "bob",
                1,
                now() - Duration::days(30),
                Some(now() - Duration::days(7)),
                false,
            ),
            // Ripened five days ago — the oldest item.
            token("carol", 0, now() - Duration::days(8), None, false),
        ];

        let whats: Vec<String> = derive_follow_ups(&audience, "room-7c2f", &tokens, &policy, now())
            .into_iter()
            .map(|obligation| obligation.what)
            .collect();
        assert_eq!(
            whats,
            vec![
                "verify delivery of data room 'room-7c2f' to carol: never opened; confirm the \
                 link arrived or try another channel"
                    .to_string(),
                "follow up with bob on data room 'room-7c2f': opened, no reply".to_string(),
                "verify delivery of data room 'room-7c2f' to alice: never opened; confirm the \
                 link arrived or try another channel"
                    .to_string(),
            ]
        );
    }

    /// Pins the reply-after-recording leak: a reply suppresses only FUTURE
    /// derivation, so a follow-up already recorded from the pre-reply snapshot
    /// stayed open in the register forever. The diff returns exactly the old
    /// tuple — and nothing else — for the caller to settle as Released.
    #[test]
    fn a_reply_after_ripening_yields_exactly_the_recorded_tuple_to_settle() {
        let (audience, policy) = fixture();
        let last_seen = now() - Duration::days(6);
        let previous = vec![token(
            "bob",
            1,
            now() - Duration::days(10),
            Some(last_seen),
            false,
        )];
        let mut replied = previous.clone();
        replied[0].replied = true;

        let to_settle =
            follow_ups_to_settle(&audience, "room-7c2f", &previous, &replied, &policy, now());
        assert_eq!(to_settle.len(), 1);
        assert_eq!(
            to_settle[0].what,
            "follow up with bob on data room 'room-7c2f': opened, no reply"
        );
        assert_eq!(to_settle[0].due_at, last_seen + Duration::days(5));
        assert_eq!(to_settle[0].audience, audience);
        // The current snapshot derives nothing: settling is all that remains.
        assert!(derive_follow_ups(&audience, "room-7c2f", &replied, &policy, now()).is_empty());
    }

    /// Pins the per-cycle flooding leak: an advancing `last_seen` mints a NEW
    /// tuple per visit-then-silence cycle (a later due date is a new derived
    /// id) while the superseded row stayed open forever. The diff yields the
    /// OLD tuple to settle while derive_follow_ups on the current snapshot
    /// yields the new one — the register swaps rows instead of accumulating
    /// them. The two tuples share their text, so `due_at` alone tells them
    /// apart: identity must include it.
    #[test]
    fn an_advanced_last_seen_yields_the_old_tuple_and_the_current_sweep_the_new() {
        let (audience, policy) = fixture();
        let old_last_seen = now() - Duration::days(12);
        let new_last_seen = now() - Duration::days(6);
        let previous = vec![token(
            "bob",
            1,
            now() - Duration::days(20),
            Some(old_last_seen),
            false,
        )];
        let current = vec![token(
            "bob",
            2,
            now() - Duration::days(20),
            Some(new_last_seen),
            false,
        )];

        let to_settle =
            follow_ups_to_settle(&audience, "room-7c2f", &previous, &current, &policy, now());
        assert_eq!(to_settle.len(), 1);
        assert_eq!(
            to_settle[0].what,
            "follow up with bob on data room 'room-7c2f': opened, no reply"
        );
        assert_eq!(
            to_settle[0].due_at,
            old_last_seen + Duration::days(5),
            "the OLD basis is what gets settled"
        );

        let current_sweep = derive_follow_ups(&audience, "room-7c2f", &current, &policy, now());
        assert_eq!(current_sweep.len(), 1);
        assert_eq!(
            current_sweep[0].due_at,
            new_last_seen + Duration::days(5),
            "the NEW basis is what gets recorded"
        );
        assert_eq!(
            current_sweep[0].what, to_settle[0].what,
            "same text either side — only due_at separates the tuples"
        );
    }

    /// Nothing ripened from the previous snapshot means the recording half
    /// never produced those rows, so there is nothing to settle — the diff
    /// must not invent settlements for rows that were never recordable. And an
    /// unchanged ripened snapshot settles nothing either: its tuple is still
    /// derivable, and settling it would fight the recording half.
    #[test]
    fn nothing_ripened_previously_and_nothing_changed_both_settle_nothing() {
        let (audience, policy) = fixture();

        // Inside the window at `now`: previous sweeps recorded nothing, so a
        // reply arriving now has nothing to release.
        let previous = vec![token("alice", 0, now() - Duration::days(1), None, false)];
        let mut replied = previous.clone();
        replied[0].replied = true;
        assert_eq!(
            follow_ups_to_settle(&audience, "room-7c2f", &previous, &replied, &policy, now()).len(),
            0
        );

        // Ripened but unchanged: still derivable, so never settled.
        let ripened = vec![token(
            "bob",
            1,
            now() - Duration::days(10),
            Some(now() - Duration::days(6)),
            false,
        )];
        assert_eq!(
            follow_ups_to_settle(&audience, "room-7c2f", &ripened, &ripened, &policy, now()).len(),
            0
        );
    }

    /// The settle handle is rebuilt from the tuple, so every field must
    /// byte-match what derive_follow_ups(previous) produced — any drift (a
    /// decorated text, a nudged due date) would rebuild a DIFFERENT register
    /// id and the settlement would release nothing.
    #[test]
    fn returned_tuples_byte_match_the_previous_derivation() {
        let (audience, policy) = fixture();
        let previous = vec![
            // A ripe delivery question and a ripe follow-up.
            token("alice", 0, now() - Duration::days(8), None, false),
            token(
                "bob",
                1,
                now() - Duration::days(30),
                Some(now() - Duration::days(7)),
                false,
            ),
        ];
        // Everyone replied: every previously derivable tuple must come back.
        let current: Vec<TokenContext> = previous
            .iter()
            .cloned()
            .map(|mut context| {
                context.replied = true;
                context
            })
            .collect();

        let recorded = derive_follow_ups(&audience, "room-7c2f", &previous, &policy, now());
        let to_settle =
            follow_ups_to_settle(&audience, "room-7c2f", &previous, &current, &policy, now());
        assert_eq!(recorded.len(), 2);
        assert_eq!(to_settle.len(), 2);
        for (was_recorded, settle) in recorded.iter().zip(&to_settle) {
            assert_eq!(settle.audience, was_recorded.audience);
            assert_eq!(settle.program_id, was_recorded.program_id);
            assert_eq!(settle.what, was_recorded.what);
            assert_eq!(settle.due_at, was_recorded.due_at);
            assert_eq!(settle.direction, was_recorded.direction);
            assert_eq!(settle.created_by, was_recorded.created_by);
            assert_eq!(settle.source_act_ref, was_recorded.source_act_ref);
        }
    }
}
