//! Turning what a provider told us into who we may no longer contact.
//!
//! `magician_v2::delivery` records that a message hard-bounced or drew a
//! complaint. `magician_v2::suppression` records who must not be contacted and
//! why. Both are correct alone and **neither imports the other**, deliberately:
//! a ledger of provider receipts should not hold contact policy, and a contact
//! register should not know what a webhook looks like.
//!
//! So the join had no home, and a standing review found the consequence — the
//! two modules' docs described this loop in the present tense while no code
//! path connected them, so a hard bounce reached the register nowhere. This
//! module is that home.
//!
//! # Why a sweep rather than a conversion
//!
//! The mapping itself is two arms wide and total. What actually needs owning is
//! the *operation*: read what is new since a cursor, translate, record. That is
//! state and cadence, which belong to a coordinator, not to a type — the same
//! shape `data_room::sweep` and `outcome_learning::sweep` already take here.
//!
//! The cursor is a **performance** concern, not a correctness one.
//! [`SuppressionRegister::ingest`] is idempotent per
//! `(identity, reason, evidence)`, so a sweep that loses its place re-ingests
//! and changes nothing. Losing the cursor costs work, never truth.
//!
//! # What this deliberately does not do
//!
//! It does not lift. A bounce that stops bouncing is not evidence that an
//! address is live again, and `OptOut`/`Complaint` cannot be lifted by anything
//! except an explicit owner act — that rule lives in `suppression` and this
//! module has no business softening it.
//!
//! # Who runs it
//!
//! [`worker::SuppressionSweepWorker::spawn`], on the cadence
//! `magician-config.yaml`'s `delivery_hygiene` key sets. That is the named
//! entry point, and it is the whole reason this module exists rather than
//! sitting beside the other sweeps nobody calls.
//!
//! [`worker::SuppressionSweepWorker::spawn_with_receipts`] is the same start
//! with a receipt source attached, and it is the named entry point for the pull
//! described below. `spawn` is it with `None`, which is what the binary calls
//! today — reported honestly as `receipts_state: "no_source"` rather than as a
//! quiet tick.
//!
//! # The other half: what the ledger never heard back about
//!
//! [`silence`] is the second question this worker's tick asks of the same
//! ledger. Hygiene above is *"a provider told us something bad, who may we no
//! longer contact"*; silence is *"a provider told us nothing at all, and for
//! how long"*. They share the tick because they share the ledger and the same
//! per-scope failure mode — and because a rising unacknowledged count is the
//! only early warning that a rail has silently stopped answering, which is a
//! signal that has to arrive without anyone having asked for it.
//!
//! Nothing in [`silence`] writes. It reads the dispatch log, reads the ledger,
//! and derives every number from the clock, so a failure there is reported and
//! cannot corrupt the register this module's own sweep fills.
//!
//! # The third half: where a receipt gets in at all
//!
//! The sweep above consumes what the ledger holds and the watch counts what it
//! never heard. Both were reading a ledger **nothing wrote to**:
//! `DeliveryLedger::reconcile` had no production caller anywhere, so the
//! register this module fills was filled from an empty source and every live
//! send stayed at `dispatch_unknown` forever.
//!
//! `delivery::intake::ReceiptIntake::admit` then built the door, and it is a
//! **push** surface: it works when somebody is holding a receipt and walks it
//! through. Nothing here holds one unprompted.
//!
//! [`receipts`] is the **pull**. It asks a
//! [`ReceiptPuller`](receipts::ReceiptPuller) what it is holding, walks each
//! receipt through that same door, and moves the outward disclosure off
//! `dispatch_unknown` where — and only where — the receipts prove it. The
//! puller is a **port**: a DSN bridge reading a bounce mailbox, a webhook
//! queue, a Kapso poller and an operator's outbox are all the same shape from
//! here, and nothing in this subsystem learns any of their names. The one
//! implementor today is `delivery_receipts::pull::DsnPuller`, which lives on
//! the rail-specific side — specific imports generic, never back.
//!
//! It rides the same tick, immediately **before** the sweep, so a bounce pulled
//! in at 09:00 suppresses at 09:00 rather than waiting a whole interval. And it
//! writes nothing to the register and nothing directly into the ledger: there
//! is one door into the ledger and one route from a provider receipt to a
//! suppression entry, and both are the ones already here.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::{DeliveryLedger, DeliveryScope, SuppressionCause};
use crate::magician_v2::suppression::{
    SuppressionEvidence, SuppressionReason, SuppressionRegister, SuppressionScope,
    SuppressionSignal,
};

pub mod receipts;
pub mod silence;
pub mod worker;

/// Who recorded the suppression, as an auditor reads it.
const RECORDED_BY: &str = "delivery-hygiene-sweep";

/// Field separator for derived ids across this codebase. A caller string that
/// feeds one — or that feeds a path segment beside one — is refused if it holds
/// this character, here as everywhere else.
const FIELD_SEP: char = '\u{1f}';

/// What one pass did, in counts.
///
/// Counts, never rates — a rate here would invite "97% deliverable" on a sample
/// of three.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HygieneSweep {
    /// Signals the ledger offered.
    pub considered: usize,
    /// Identities newly suppressed by this pass.
    pub suppressed: usize,
    /// Signals that named an identity already suppressed for that reason.
    pub already_held: usize,
    /// Signals the register could not read, and so did not act on.
    ///
    /// Carried so the three outcomes ADD UP to `considered`. Without it a pass
    /// that could read none of five hundred signals reported *"considered 500,
    /// suppressed 0, already held 0"* — three true numbers describing a sweep
    /// that had silently done nothing.
    pub unreadable: usize,
}

impl HygieneSweep {
    /// Whether this pass changed anything.
    ///
    /// A pass that skipped signals is NOT quiet even when it suppressed
    /// nothing: quiet means the ledger had nothing new to say, and that is the
    /// opposite of a pass that had something to say and could not read it.
    pub fn is_quiet(&self) -> bool {
        self.suppressed == 0 && self.unreadable == 0
    }

    /// How many of the signals offered ended under a named outcome.
    ///
    /// Equal to `considered` by construction, and asserted against it in tests
    /// so it stays that way as the sweep grows an outcome. A gap would mean
    /// signals the ledger offered that this report cannot say what became of.
    pub fn accounted_for(&self) -> usize {
        self.suppressed + self.already_held + self.unreadable
    }
}

/// The whole mapping, in one place, exhaustive by construction.
///
/// A new `SuppressionCause` arm will fail to compile here rather than silently
/// reaching the register as nothing. That is the point of matching rather than
/// deriving: the compiler asks the policy question.
///
/// `delivery` deliberately emits no soft-bounce cause, so the decision that a
/// soft bounce does NOT suppress is already made upstream and is not re-made
/// here.
fn reason_for(cause: SuppressionCause) -> SuppressionReason {
    match cause {
        SuppressionCause::HardBounce => SuppressionReason::HardBounce,
        SuppressionCause::Complaint => SuppressionReason::Complaint,
    }
}

/// Translate what the ledger has seen since `since` into register signals.
///
/// Pure: it reads, it converts, it returns intent. The caller decides whether
/// to record — which is what makes this testable without a register.
pub fn signals_since(
    ledger: &DeliveryLedger,
    delivery_scope: &DeliveryScope,
    since: DateTime<Utc>,
) -> Result<Vec<SuppressionSignal>> {
    let observed = ledger
        .suppression_signals(delivery_scope, since)
        .context("reading delivery suppression signals")?;

    Ok(observed
        .into_iter()
        .map(|(identity, cause)| SuppressionSignal {
            identity,
            reason: reason_for(cause),
            // The evidence ref points back at the ledger, which is where an
            // auditor asking "why is this address suppressed" has to end up.
            // `since` is in it because that is what makes the pass locatable.
            evidence: SuppressionEvidence::new(
                since,
                format!(
                    "delivery:{}:{}:since:{}",
                    delivery_scope.principal,
                    delivery_scope.workspace,
                    since.to_rfc3339()
                ),
                RECORDED_BY,
            ),
        })
        .collect())
}

/// Read the ledger, record what it found, report counts.
///
/// The register is the authority on what is new: `ingest` returns only the
/// suppressions it actually wrote, so `already_held` is derived from the
/// difference rather than guessed at here.
///
/// # The two scopes must name the same owner
///
/// They are separate parameters because they are separate modules' types, not
/// because they may differ. A sweep handed one tenant's ledger and another
/// tenant's register would write the first owner's bounces into the second
/// owner's register — suppressing people the second owner never mailed, on
/// evidence that points at a ledger they cannot read. It is refused, in the
/// same shape `outcome_learning::sweep` refuses its own crossed scopes.
pub fn sweep_delivery_into_suppression(
    ledger: &DeliveryLedger,
    delivery_scope: &DeliveryScope,
    register: &SuppressionRegister,
    suppression_scope: &SuppressionScope,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<HygieneSweep> {
    if delivery_scope.principal != suppression_scope.principal
        || delivery_scope.workspace != suppression_scope.workspace
    {
        anyhow::bail!(
            "refusing to sweep `{}/{}`'s delivery ledger into `{}/{}`'s suppression register: \
             one owner's bounces are not evidence about another owner's recipients, and the \
             evidence ref would point at a ledger the second owner cannot read",
            delivery_scope.principal,
            delivery_scope.workspace,
            suppression_scope.principal,
            suppression_scope.workspace
        );
    }

    let signals = signals_since(ledger, delivery_scope, since)?;
    if signals.is_empty() {
        return Ok(HygieneSweep::default());
    }

    let written = register
        .ingest(suppression_scope, &signals, now)
        .context("recording delivery signals in the suppression register")?;

    // `newly_recorded`, not `recorded.len()`. The register returns every
    // signal's entry whether it wrote it or resumed one already on file, so
    // counting the vector reported the same bounce as a fresh suppression on
    // every tick and left `already_held` permanently zero — a sweep that looked
    // busy forever over a ledger nothing had changed.
    Ok(HygieneSweep {
        considered: signals.len(),
        suppressed: written.newly_recorded,
        already_held: written.already_held(),
        unreadable: written.unreadable.len(),
    })
}

// ── The cursor ──────────────────────────────────────────────────────────────

/// One line of a scope's cursor log.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorRow {
    /// The instant the sweep that wrote this line began reading.
    swept_through: DateTime<Utc>,
}

/// Where each owner's last completed sweep read up to.
///
/// # Losing this costs work, never truth
///
/// [`SuppressionRegister::ingest`] is idempotent per
/// `(identity, reason, evidence)`, so a sweep that re-reads ground it already
/// covered writes nothing new: it resumes the rows it wrote last time and
/// reports them as `already_held`. Delete this log and the next tick re-reads
/// the whole ledger, spends the IO, and lands on exactly the register it would
/// have had. That is why it is a bookmark in its own file rather than a field
/// on anything that matters.
///
/// # An absent cursor means the beginning of time, never "now"
///
/// [`Self::read_from`] answers [`DateTime::<Utc>::MIN_UTC`] when nothing has
/// ever been swept. Defaulting to the clock instead would be the cheap answer
/// and the wrong one: every bounce and every complaint recorded before the
/// worker was first started would be skipped permanently, silently, on the one
/// tick where the register is emptiest — which is precisely the vacuous-screen
/// failure this whole sweep exists to end.
///
/// # Monotone
///
/// [`Self::advance_to`] never moves a cursor backwards. A stale tick finishing
/// after a newer one must not undo its progress; the older value is a no-op
/// rather than an error, because "read from at least here" is still honoured by
/// the newer value it declines to overwrite.
#[derive(Debug, Clone)]
pub struct HygieneCursor {
    workspace_layout: ArtifactV2Workspace,
}

impl HygieneCursor {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// What the last completed sweep read up to, or `None` if none ever has.
    ///
    /// The maximum of every line, not the last one. Order is what a rewrite or
    /// a concurrent append could disturb, and a cursor that a torn ordering
    /// could walk backwards would re-offer work forever without anybody
    /// noticing the log was wrong.
    ///
    /// An unreadable log is an error, never a missing cursor. Reading it as
    /// absent would silently restart the sweep from the beginning of time on
    /// every tick — wrong in the safe direction, but wrong loudly enough that
    /// it should be said rather than absorbed.
    pub fn swept_through(&self, scope: &SuppressionScope) -> Result<Option<DateTime<Utc>>> {
        let path = self.cursor_path(scope)?;
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(None);
        };
        Ok(
            crate::magician_v2::jsonl::parse_log_lines::<CursorRow>(&raw, &path)?
                .into_iter()
                .map(|row| row.swept_through)
                .max(),
        )
    }

    /// The instant the next sweep should read from.
    ///
    /// `MIN_UTC` when the cursor is absent — see the type's own docs for why
    /// that is not `now`.
    pub fn read_from(&self, scope: &SuppressionScope) -> Result<DateTime<Utc>> {
        Ok(self
            .swept_through(scope)?
            .unwrap_or(DateTime::<Utc>::MIN_UTC))
    }

    /// Move the cursor forward, returning where it now stands.
    ///
    /// Called **after** the ingest it bookmarks, never before. Record-before-act
    /// governs records of acts; this is a bookmark, and a bookmark written
    /// before the reading is a bookmark that lies — a crash between the write
    /// and the ingest would skip every signal in the window it claimed.
    pub fn advance_to(
        &self,
        scope: &SuppressionScope,
        through: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let path = self.cursor_path(scope)?;
        if let Some(held) = self.swept_through(scope)? {
            if through <= held {
                return Ok(held);
            }
        }
        let mut line = serde_json::to_vec(&CursorRow {
            swept_through: through,
        })?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending cursor {}", path.display()))?;
        Ok(through)
    }

    fn cursor_path(&self, scope: &SuppressionScope) -> Result<PathBuf> {
        if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
            anyhow::bail!(
                "a scope's principal and workspace must not contain U+001F: it is the separator \
                 that keeps derived components from bleeding into each other, and a crafted \
                 scope could otherwise read another owner's cursor"
            );
        }
        Ok(self
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("delivery_hygiene")
            .join("cursor.jsonl"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeZone;

    use crate::magician_v2::delivery::{DeliveryReceipt, DeliveryState};

    fn t(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0)
            .single()
            .expect("a real instant")
    }

    fn receipt(identity: &str, state: DeliveryState, observed: DateTime<Utc>) -> DeliveryReceipt {
        DeliveryReceipt {
            provider: "postal".to_string(),
            provider_message_id: format!("pm-{}", state.as_str()),
            identity: identity.to_string(),
            state,
            observed_at: observed,
            payload_ref: format!("hook-{}", state.as_str()),
        }
    }

    fn scopes() -> (DeliveryScope, SuppressionScope) {
        (
            DeliveryScope::new("anonymous", "default"),
            SuppressionScope::new("anonymous", "default"),
        )
    }

    /// A hard bounce recorded in the ledger must reach the register, and the
    /// entry must be the one the send-time screen then blocks on.
    ///
    /// Pins the failure the whole programme is named after: `ingest` had no
    /// non-test caller, so the register stayed empty and
    /// `outward_gate::contact_refusal` screened every recipient against
    /// nothing. The assertion below is deliberately in two halves — the signal
    /// EXISTS in the ledger first, then it is found in the register — because a
    /// test that only asserted the second half would pass against a ledger that
    /// silently returned nothing.
    #[test]
    fn a_hard_bounce_in_the_ledger_reaches_the_register() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (delivery_scope, suppression_scope) = scopes();
        let ledger = DeliveryLedger::new(layout.clone());
        let register = SuppressionRegister::global(layout.clone());

        ledger
            .reconcile(
                &delivery_scope,
                "act-1",
                &receipt(
                    "Dead@Example.Test",
                    DeliveryState::Bounced { hard: true },
                    t(8),
                ),
                t(9),
            )
            .expect("the provider receipt records");

        // The ledger really holds it. Without this the sweep assertion below
        // would pass over an empty read.
        assert_eq!(
            ledger
                .suppression_signals(&delivery_scope, DateTime::<Utc>::MIN_UTC)
                .expect("signals"),
            vec![(
                "dead@example.test".to_string(),
                SuppressionCause::HardBounce
            )]
        );

        let swept = sweep_delivery_into_suppression(
            &ledger,
            &delivery_scope,
            &register,
            &suppression_scope,
            DateTime::<Utc>::MIN_UTC,
            t(10),
        )
        .expect("the sweep runs");
        assert_eq!(swept.considered, 1);
        assert_eq!(
            swept.accounted_for(),
            swept.considered,
            "every signal offered must end under exactly one named outcome"
        );
        assert_eq!(swept.suppressed, 1);
        assert_eq!(swept.already_held, 0);

        // Asserted through the send-time call rather than through `history`,
        // through the method-form call that the workspace scan in
        // `suppression`'s own tests sees: whether a module screens recipients
        // must be a fact about the code, not about where a line happened to
        // wrap.
        let identities = vec!["dead@example.test".to_string()];
        let screened = register
            .screen(&suppression_scope, &identities, t(11))
            .expect("the screen reads");
        assert_eq!(screened.sendable, Vec::<String>::new());
        assert_eq!(screened.blocked.len(), 1);
        assert_eq!(screened.blocked[0].reason, SuppressionReason::HardBounce);
        assert_eq!(screened.blocked[0].identity, "dead@example.test");
    }

    /// Re-running the same window writes nothing new, which is what makes a
    /// lost cursor cost work rather than truth.
    #[test]
    fn re_sweeping_the_same_window_records_nothing_new() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (delivery_scope, suppression_scope) = scopes();
        let ledger = DeliveryLedger::new(layout.clone());
        let register = SuppressionRegister::global(layout.clone());

        ledger
            .reconcile(
                &delivery_scope,
                "act-1",
                &receipt("angry@example.test", DeliveryState::Complained, t(8)),
                t(9),
            )
            .expect("the provider receipt records");

        let first = sweep_delivery_into_suppression(
            &ledger,
            &delivery_scope,
            &register,
            &suppression_scope,
            DateTime::<Utc>::MIN_UTC,
            t(10),
        )
        .expect("first sweep");
        assert_eq!(first.suppressed, 1);

        let replay = sweep_delivery_into_suppression(
            &ledger,
            &delivery_scope,
            &register,
            &suppression_scope,
            DateTime::<Utc>::MIN_UTC,
            t(12),
        )
        .expect("second sweep over the same window");
        assert_eq!(replay.considered, 1);
        assert_eq!(
            replay.accounted_for(),
            replay.considered,
            "every signal offered must end under exactly one named outcome"
        );
        assert_eq!(
            replay.suppressed, 0,
            "an identical replay must resume the row it already wrote"
        );
        assert_eq!(replay.already_held, 1);

        // And exactly one entry exists, keeping its original recording time.
        let history = register
            .history(&suppression_scope, "angry@example.test")
            .expect("history reads");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].suppressed_at, t(10));
    }

    /// One owner's bounces must never land in another owner's register.
    ///
    /// The two scope parameters exist because they are two modules' types, not
    /// because they may differ — and nothing in the signature says so. Without
    /// this guard a caller that built them from different sources would
    /// suppress people the second owner never contacted, on evidence pointing
    /// at a ledger they cannot open.
    #[test]
    fn a_sweep_across_two_owners_is_refused() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let delivery_scope = DeliveryScope::new("alpha", "prod");
        let ledger = DeliveryLedger::new(layout.clone());
        let register = SuppressionRegister::global(layout.clone());

        ledger
            .reconcile(
                &delivery_scope,
                "act-1",
                &receipt(
                    "dead@example.test",
                    DeliveryState::Bounced { hard: true },
                    t(8),
                ),
                t(9),
            )
            .expect("the provider receipt records");

        let foreign = SuppressionScope::new("beta", "prod");
        let error = sweep_delivery_into_suppression(
            &ledger,
            &delivery_scope,
            &register,
            &foreign,
            DateTime::<Utc>::MIN_UTC,
            t(10),
        )
        .expect_err("a crossed sweep must be refused");
        assert!(error.to_string().contains("refusing to sweep"), "{error}");

        // And nothing was written to the other owner's register.
        assert_eq!(
            register
                .history(&foreign, "dead@example.test")
                .expect("history reads"),
            Vec::new()
        );
    }

    /// An absent cursor is the beginning of time, never the clock.
    ///
    /// Pins the miss that would be invisible: defaulting to `now` on the first
    /// tick skips every bounce and complaint recorded before the worker was
    /// first started, permanently, and the register that results looks exactly
    /// like a register with nothing to hold.
    #[test]
    fn an_absent_cursor_reads_from_the_beginning_of_time() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (_delivery_scope, suppression_scope) = scopes();
        let cursor = HygieneCursor::new(layout);

        assert_eq!(
            cursor.swept_through(&suppression_scope).expect("reads"),
            None
        );
        assert_eq!(
            cursor.read_from(&suppression_scope).expect("reads"),
            DateTime::<Utc>::MIN_UTC
        );
    }

    /// A cursor moves forward and stays there; a stale tick cannot walk it back.
    #[test]
    fn a_cursor_is_monotone_and_survives_a_stale_tick() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (_delivery_scope, suppression_scope) = scopes();
        let cursor = HygieneCursor::new(layout);

        assert_eq!(
            cursor.advance_to(&suppression_scope, t(9)).expect("first"),
            t(9)
        );
        assert_eq!(cursor.read_from(&suppression_scope).expect("reads"), t(9));
        assert_eq!(
            cursor
                .advance_to(&suppression_scope, t(12))
                .expect("forward"),
            t(12)
        );
        assert_eq!(
            cursor.advance_to(&suppression_scope, t(10)).expect("stale"),
            t(12),
            "a stale tick must not undo a newer tick's progress"
        );
        assert_eq!(cursor.read_from(&suppression_scope).expect("reads"), t(12));
    }

    /// An unreadable cursor is an error, never a missing one.
    ///
    /// The safe reading either way is "sweep from the beginning", so the cost
    /// of getting this wrong is silence rather than a miss — which is exactly
    /// why it has to be said out loud instead of absorbed into a default.
    #[test]
    fn an_unreadable_cursor_is_an_error_never_an_absent_one() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let (_delivery_scope, suppression_scope) = scopes();
        let cursor = HygieneCursor::new(layout);
        cursor
            .advance_to(&suppression_scope, t(9))
            .expect("a cursor exists");

        let path = cursor.cursor_path(&suppression_scope).expect("a path");
        std::fs::remove_file(&path).expect("remove the cursor log");
        std::fs::create_dir_all(&path).expect("a directory in its place");

        let error = cursor
            .swept_through(&suppression_scope)
            .expect_err("an unreadable cursor must not read as an absent one");
        assert!(error.to_string().contains("unreadable"), "{error}");
    }

    /// A scope carrying the id separator is refused before it reaches a path.
    #[test]
    fn a_scope_holding_the_unit_separator_cannot_read_a_cursor() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        let cursor = HygieneCursor::new(layout);
        let crafted = SuppressionScope::new("anonymous\u{1f}beta", "default");

        let error = cursor
            .swept_through(&crafted)
            .expect_err("U+001F must be refused");
        assert!(error.to_string().contains("U+001F"), "{error}");
    }
}
