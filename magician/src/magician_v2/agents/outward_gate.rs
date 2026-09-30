//! The questions an outward act must answer before anything leaves, and the
//! record of the ones that did leave.
//!
//! # Why this is a module and not four more blocks in the executor
//!
//! `execute_action_inner` is the one function every action passes through, so
//! it is the only honest place for an outward gate — but the gate's *decisions*
//! are ordinary predicates over ordinary values, and predicates buried inside a
//! forty-thousand-line async function cannot be tested without standing up an
//! execution. Everything here takes plain arguments and returns a plain answer,
//! so each refusal below is pinned by a test that runs in milliseconds.
//!
//! # Generic, not one flow's machinery
//!
//! A recipient, a capability name, a work context, an act ref, a clock. Nothing
//! here knows what is being sent or what programme it serves. Mail, chat,
//! calendar invitations and rails that do not exist yet ask the same three
//! questions:
//!
//! 1. **May this actor use this capability inside this work at all?**
//!    [`work_context_refusal`].
//! 2. **May we contact this person at all?** [`contact_refusal`].
//! 3. **Did the act that left ever get acknowledged?** [`DispatchLog`], which
//!    supplies [`DeliveryLedger::unreconciled`] the candidate list it documents
//!    as *"supplied, never discovered"*.
//!
//! Questions 1 and 2 are asked on every dispatch the executor classifies as
//! outward. Question 3 is now asked as well: [`DispatchLog::record`] runs at
//! every live send, and `delivery_hygiene::silence` reads the pair back —
//! from the delivery-hygiene worker's tick and from
//! `GET /api/magician/v2/delivery/unacknowledged`. Nothing yet records a
//! provider **receipt** against what it returns, so today every dispatched act
//! is unacknowledged; that is the fact the read reports rather than one it
//! hides.
//!
//! # Fail closed, everywhere
//!
//! Every refusal below fires on absence as readily as on a positive finding. An
//! unreadable register, an engagement the store cannot answer for and a missing
//! scope are all refusals, because each of them is the runtime saying *"I could
//! not check"* — and "I could not check" is never permission.
//!
//! An empty recipient list is a refusal too, but only where empty means *we
//! failed to read who this reaches*. [`Addressing::for_class`] splits those
//! classes from the ones where empty is a fact about the act — a calendar entry
//! on your own day, a form addressed to a site — and for those
//! [`contact_refusal`] returns `None` without opening the register at all. That
//! is the one path to a send that the suppression check does not cover, and it
//! is derived from the capability's class, never from the arguments.
//!
//! The one place that says *nothing* rather than *yes* is an execution carrying
//! no work context at all, and it says nothing explicitly
//! ([`WorkBinding::Unbound`]) rather than by returning a clear verdict it has no
//! basis for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::outward_actions::OutwardClass;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::{DeliveryLedger, DeliveryScope, DispatchedAct, UnreconciledAct};
use crate::magician_v2::suppression::{SuppressionRegister, SuppressionScope};
use crate::magician_v2::work_context::{self, CapabilityStanding, WorkContext};

/// Field separator for derived ids across this codebase. A caller string that
/// feeds one is refused if it holds this character, here as everywhere else.
const FIELD_SEP: char = '\u{1f}';

/// Every refusal opens the same way, because the model must never have to
/// notice which gate refused in order to understand that nothing was sent.
const REFUSAL_PREFIX: &str = "NOT SENT — this outward action was refused.";

// ── 1. May we contact this person at all? ───────────────────────────────────

/// Whether an empty recipient list is a parse failure or a real "reaches nobody".
///
/// The distinction exists because the same empty list means opposite things for
/// different capabilities, and the fail-closed reading is only correct for one
/// of them. A message with no extractable recipient is a message we cannot
/// account for. A calendar entry with no attendees is an entry on your own
/// calendar. Callers derive this from the capability, never from the arguments —
/// arguments are what we just failed to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// The act transmits to named people; an empty list is a failure to parse.
    RecipientRequired,
    /// The act may carry people but is meaningful without them.
    MayReachNobody,
}

impl Addressing {
    /// Derive from the outward class — the capability's own classification, and
    /// never from the arguments, which are what we just failed to read.
    ///
    /// Mail and messages exist to reach a named person: an empty list there is a
    /// parse failure. A calendar entry is meaningful with nobody but the owner
    /// on it, so an empty attendee list is a fact about the act, not a gap in
    /// our reading of it.
    pub fn for_class(class: OutwardClass) -> Self {
        match class {
            OutwardClass::Mail | OutwardClass::Message => Self::RecipientRequired,
            OutwardClass::CalendarInvite => Self::MayReachNobody,
            // A form submission is addressed to a site, not to a person: there
            // is no recipient list to fail to parse, so an empty one is a fact
            // about the act rather than a gap in our reading of it. Classing it
            // `RecipientRequired` would refuse every submission at the
            // suppression screen — a register of people the owner may not
            // contact has nothing to say about a web form — and a gate that
            // refuses one hundred percent of a capability is a gate somebody
            // routes around. The capture posture, the envelope and the
            // disclosure record still apply, which is where a submission is
            // actually held.
            OutwardClass::FormSubmission => Self::MayReachNobody,
        }
    }
}

/// Screen an outward act's recipients against the owner's suppression register.
///
/// `Some(message)` is a refusal carrying what the model needs to understand it;
/// `None` means every recipient came back clear and the act may continue to the
/// next gate.
///
/// # Every "I cannot check" branch refuses
///
/// - **No recipients.** An act whose audience cannot be named is refused rather
///   than screened over nobody. [`SuppressionRegister::screen`] refuses an empty
///   list for exactly this reason, and returning `None` before reaching it would
///   have reinstated the vacuous pass it exists to prevent.
/// - **No scoped store.** With no principal, workspace or artifact workspace
///   there is no register to consult. An unconsultable register is not an empty
///   one.
/// - **An unreadable register.** [`SuppressionRegister::is_suppressed`] states
///   the contract outright: `Err` means DO NOT SEND. A malformed recipient lands
///   here too — "we could not parse who this reaches" is not permission to reach
///   them.
/// - **Nobody cleared.** The verdict is read off `sendable`, never off
///   `blocked.is_empty()`, so a screen that somehow blocked nobody *and* cleared
///   nobody refuses instead of passing.
pub fn contact_refusal(
    workspace_layout: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace: Option<&str>,
    recipients: &[String],
    addressing: Addressing,
    now: DateTime<Utc>,
) -> Option<String> {
    if recipients.is_empty() {
        // An empty list means one of two different things, and treating them
        // alike is how a correct guard becomes an over-gate. For a message,
        // empty means we could not tell who it reaches, which is refused. For
        // an act that MAY carry people but need not — a calendar entry with no
        // attendees — empty means it reaches nobody, and there is nobody to
        // screen. Refusing that would block booking one's own time, which is
        // the flow `scheduling::consumer` depends on. The act is still recorded
        // as a disclosure either way; only the screen is skipped.
        return match addressing {
            Addressing::RecipientRequired => Some(format!(
                "{REFUSAL_PREFIX} No recipient could be extracted from this action's arguments, \
                 so the suppression register could not be consulted for anybody. An outward act \
                 whose audience cannot be named is refused: `we could not tell who this reaches` \
                 is not permission to reach them. Re-issue it naming the recipient in a typed \
                 parameter rather than in a raw argument list."
            )),
            Addressing::MayReachNobody => None,
        };
    }

    let (Some(workspace_layout), Some(principal), Some(workspace)) =
        (workspace_layout, principal, workspace)
    else {
        return Some(format!(
            "{REFUSAL_PREFIX} This execution carries no scoped store, so the owner's suppression \
             register could not be consulted at all. An unconsultable register is not an empty \
             one, and an unchecked recipient is never a permitted one."
        ));
    };

    let register = SuppressionRegister::global(workspace_layout.clone());
    let scope = SuppressionScope::new(principal, workspace);
    let screened = match register.screen(&scope, recipients, now) {
        Ok(screened) => screened,
        Err(error) => {
            return Some(format!(
                "{REFUSAL_PREFIX} The suppression register could not be read, and an unreadable \
                 register means DO NOT SEND — it is never an empty one. Nothing was attempted \
                 for any recipient. Cause: {error:#}"
            ));
        },
    };

    if !screened.blocked.is_empty() {
        // Named one by one, with the reason and the moment the reason was
        // established. A refusal that says only "suppressed" invites the model
        // to try a second address for the same person; a refusal that says
        // "they opted out on this date" does not.
        let mut named: Vec<String> = screened
            .blocked
            .iter()
            .map(|entry| {
                format!(
                    "`{}` ({}, established {})",
                    entry.identity,
                    entry.reason.as_str(),
                    entry.evidence.established_at.to_rfc3339()
                )
            })
            .collect();
        named.sort();
        return Some(format!(
            "{REFUSAL_PREFIX} {} of this action's {} recipient(s) are on the owner's suppression \
             register and must not be contacted: {}. Do not retry, do not reach the same person \
             through a different address, and do not report this as sent.",
            screened.blocked.len(),
            recipients.len(),
            named.join("; ")
        ));
    }

    if screened.sendable.is_empty() {
        return Some(format!(
            "{REFUSAL_PREFIX} The suppression screen cleared nobody for this act. A send with no \
             cleared recipient is refused rather than performed against an empty audience."
        ));
    }

    None
}

// ── 2. May this actor use this capability inside this work? ─────────────────

/// What the runtime could learn about the work an act belongs to.
///
/// Three arms, because "there is no work context" and "there is one and it could
/// not be resolved" are opposite answers that a single `Option` would render
/// identically — and rendering them identically is how an unavailable authority
/// comes to read as an unrestricted one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkBinding {
    /// The execution carries no work context. The gate has **no opinion**: there
    /// is nothing to narrow against, and saying "permitted" would be a verdict
    /// nothing produced. Every other gate still decides.
    Unbound,
    /// A resolved work context, with what the work names.
    Bound(WorkContext),
    /// The execution carries a work context and the runtime could not resolve
    /// it — an unavailable store, an unknown, expired or revoked authority. A
    /// refusal, never a pass: an authority that cannot answer has not answered
    /// yes.
    Unresolvable(String),
}

/// Refuse an act whose capability the acting agent's work context does not grant.
///
/// `agent_capabilities` is the agent's **own** grant, resolved by the caller —
/// exactly what [`work_context::resolve`] asks for. Intersection can only
/// narrow, so nothing here can hand an agent a capability it does not hold.
///
/// # The four standings, each answered on its own terms
///
/// - [`CapabilityStanding::Usable`] — held and named by the work. The only arm
///   that proceeds.
/// - [`CapabilityStanding::RequiresDelegation`] — the work needs it and the
///   agent cannot do it. Refused *with the delegation list*, because an agent
///   that cannot see what the work needs will improvise with what it has, and a
///   narrow grant that improvises produces a wrong answer instead of a handoff.
/// - [`CapabilityStanding::OutsideThisWork`] — held, and this work does not name
///   it. Refused **here**, and the module note that says a forbid-decision
///   should not exclude this standing is not being contradicted: that note
///   protects an agent's standing grant *outside* the work, and this act has no
///   outside — it is being performed inside the work, by an execution bound to
///   it. The same agent on an execution bound to nothing keeps the capability,
///   which is precisely what the note preserves.
/// - [`CapabilityStanding::Unavailable`] — neither the agent nor the work has
///   anything to do with it. Refused.
///
/// No fifth outcome is invented, and the standings are not collapsed into a
/// boolean: each refusal says which of the three it is, because "you cannot do
/// this, delegate it", "this is not what this work is for" and "you do not have
/// this at all" call for three different next moves.
pub fn work_context_refusal(
    agent_capabilities: &[String],
    binding: &WorkBinding,
    capability: &str,
) -> Option<String> {
    let work = match binding {
        WorkBinding::Unbound => return None,
        WorkBinding::Unresolvable(detail) => {
            return Some(format!(
                "{REFUSAL_PREFIX} This execution is bound to a work context that could not be \
                 resolved, so the capabilities it narrows to are unknown. An authority that \
                 cannot answer has not answered yes. Cause: {detail}"
            ));
        },
        WorkBinding::Bound(work) => work,
    };

    let standing = work_context::resolve(agent_capabilities, work, capability);
    // The one arm that permits anything, asked as the module asks it. Held apart
    // from the `match` below so the permit is a single positive test rather than
    // a case that could be widened by a later edit.
    if standing.permits_direct_use() {
        return None;
    }

    let work_key = work.kind.as_key();
    let effective = work_context::effective_capabilities(agent_capabilities, work);
    let effective_label = if effective.is_empty() {
        // Explicit, not an empty list rendered as a blank. A work context that
        // narrows to nothing is a real and reportable state, and printing it as
        // "" would read as a formatting bug rather than as the finding it is.
        "<nothing: this agent and this work share no capability>".to_string()
    } else {
        effective.join(", ")
    };

    // Exhaustive, with no `_` arm: a standing added to the module later is a
    // compile error here rather than a case that silently inherits whichever
    // behaviour the catch-all had.
    let detail = match standing {
        CapabilityStanding::Usable => {
            debug_assert!(false, "usable is returned above");
            return None;
        },
        CapabilityStanding::RequiresDelegation => {
            let needs = work_context::delegation_needs(agent_capabilities, work);
            format!(
                "`{capability}` is needed by this work and this agent does not hold it, so it \
                 must be delegated to someone who does rather than approximated. The work needs \
                 and this agent cannot do: {}.",
                needs.join(", ")
            )
        },
        CapabilityStanding::OutsideThisWork => format!(
            "`{capability}` is held by this agent but is not part of what `{work_key}` is for. \
             A work context narrows; it never widens, and using a capability the work does not \
             name would be acting outside the work this execution is bound to."
        ),
        CapabilityStanding::Unavailable => {
            format!("`{capability}` is neither held by this agent nor named by `{work_key}`.")
        },
    };

    Some(format!(
        "{REFUSAL_PREFIX} {detail} Usable inside `{work_key}`: {effective_label}."
    ))
}

// ── 3. What left, and was it ever acknowledged? ─────────────────────────────

/// One recorded dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DispatchRow {
    act_ref: String,
    dispatched_at: DateTime<Utc>,
}

/// The log of acts this runtime believes left the building.
///
/// # Why the list lives here and not in the ledger
///
/// [`DispatchedAct`] is documented as *"supplied, never discovered — the list of
/// what was dispatched belongs to whoever dispatched it"*, and that separation
/// is what lets a second outward rail reconcile through the same ledger. Somebody
/// still has to *be* whoever dispatched it, or
/// [`DeliveryLedger::unreconciled`] has no candidates and `dispatch_unknown`
/// stays a permanent default rather than a queryable state. This is that
/// somebody, for the one dispatch gate every action passes through.
///
/// # Storage
///
/// One append-only log per scope, folded on read, through
/// [`crate::magician_v2::jsonl`] — so a genuinely absent log reads as "nothing
/// has been dispatched yet" and every other fault propagates. It sits beside the
/// ledger it feeds, under the same `delivery/` root, because a dispatch list
/// kept somewhere else is a dispatch list that drifts from the receipts it is
/// compared against.
#[derive(Debug, Clone)]
pub struct DispatchLog {
    workspace_layout: ArtifactV2Workspace,
}

impl DispatchLog {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn path(&self, scope: &DeliveryScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("delivery")
            .join("dispatched.jsonl")
    }

    /// Record that an act was dispatched.
    ///
    /// Called **before** the side effect, not after: an act missing from this
    /// log is invisible to every later sweep, permanently, whereas an act
    /// recorded here that then failed to leave shows up as unreconciled and gets
    /// investigated. Of the two, only the first is silent.
    ///
    /// # Replay resumes; a changed payload is an error
    ///
    /// The identical `(act_ref, dispatched_at)` again is one record — a retried
    /// dispatch of the same act must not double-count. The **same act ref with a
    /// different dispatch time** is refused rather than quietly kept or quietly
    /// overwritten: two different accounts of when one act left mean somebody
    /// upstream is reusing act refs, and swallowing the second would hide it
    /// until the silence window mattered.
    pub fn record(
        &self,
        scope: &DeliveryScope,
        act_ref: &str,
        dispatched_at: DateTime<Utc>,
    ) -> Result<()> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;

        let already_recorded = self.fold(scope)?.get(&act_ref).copied();
        if let Some(held) = already_recorded {
            if held == dispatched_at {
                return Ok(());
            }
            anyhow::bail!(
                "act `{act_ref}` is already recorded as dispatched at {}, and this call says \
                 {}; an identical replay resumes, but two different accounts of when one act \
                 left must be reconciled rather than silently dropped",
                held.to_rfc3339(),
                dispatched_at.to_rfc3339()
            );
        }

        let path = self.path(scope);
        let row = DispatchRow {
            act_ref,
            dispatched_at,
        };
        let line = serde_json::to_vec(&row).context("serialising a dispatch record")?;
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending {}", path.display()))
    }

    /// Every act recorded as dispatched in this scope, oldest first.
    ///
    /// The candidate list [`DeliveryLedger::unreconciled`] asks for.
    pub fn dispatched(&self, scope: &DeliveryScope) -> Result<Vec<DispatchedAct>> {
        let mut out: Vec<DispatchedAct> = self
            .fold(scope)?
            .into_iter()
            .map(|(act_ref, dispatched_at)| DispatchedAct::new(act_ref, dispatched_at))
            .collect();
        out.sort_by(|left, right| {
            left.dispatched_at
                .cmp(&right.dispatched_at)
                .then_with(|| left.act_ref.cmp(&right.act_ref))
        });
        Ok(out)
    }

    /// Acts this runtime dispatched and no provider ever acknowledged.
    ///
    /// The pairing that makes `dispatch_unknown` queryable: this log supplies the
    /// candidates, the ledger supplies the receipts, and the answer is derived
    /// from the clock so nothing has to have run for an act to be overdue.
    ///
    /// A scope with no recorded dispatches propagates the ledger's own refusal
    /// rather than returning an empty list, because "nothing is unacknowledged"
    /// over zero candidates is the vacuous clean bill of health the ledger
    /// documents at length.
    pub fn unreconciled(
        &self,
        ledger: &DeliveryLedger,
        scope: &DeliveryScope,
        older_than: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<UnreconciledAct>> {
        let dispatched = self.dispatched(scope)?;
        ledger.unreconciled(scope, &dispatched, older_than, now)
    }

    /// Fold the log, refusing two different dispatch times for one act ref.
    fn fold(&self, scope: &DeliveryScope) -> Result<BTreeMap<String, DateTime<Utc>>> {
        let path = self.path(scope);
        let raw = crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?;
        let Some(raw) = raw else {
            return Ok(BTreeMap::new());
        };
        let rows = crate::magician_v2::jsonl::parse_log_lines::<DispatchRow>(&raw, &path)?;
        let mut out: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        for row in rows {
            if let Some(held) = out.get(&row.act_ref).copied() {
                if held != row.dispatched_at {
                    anyhow::bail!(
                        "act `{}` is recorded as dispatched at both {} and {} in {}; the log \
                         holds two different accounts of one act and the fold refuses to pick one",
                        row.act_ref,
                        held.to_rfc3339(),
                        row.dispatched_at.to_rfc3339(),
                        path.display()
                    );
                }
                // A duplicated identical line is one dispatch, not two. First
                // wins, exactly as the write path resumes rather than stacking.
                continue;
            }
            out.insert(row.act_ref, row.dispatched_at);
        }
        Ok(out)
    }
}

/// A scope whose components cannot shear an id derivation.
fn validate_scope(scope: &DeliveryScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator that \
             keeps derived ids' components apart, and a crafted scope could otherwise resume \
             another owner's record"
        );
    }
    Ok(())
}

/// A caller string that feeds an id derivation, checked and trimmed.
fn validated_field(value: &str, what: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{what} is required: an act nothing names can never be reconciled");
    }
    if trimmed.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other"
        );
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!(
            "{what} must not contain control characters: one arriving here means the value was \
             mis-parsed upstream, and a newline would tear the log line in two"
        );
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeZone;

    use crate::magician_v2::delivery::{DeliveryReceipt, DeliveryState};
    use crate::magician_v2::suppression::{SuppressionEvidence, SuppressionReason};
    use crate::magician_v2::work_context::WorkContextKind;

    /// A fixed clock. Every boundary below is asserted against an exact instant.
    fn t(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0)
            .single()
            .expect("a real instant")
    }

    fn workspace() -> (tempfile::TempDir, ArtifactV2Workspace) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        (tmp, layout)
    }

    /// A `Vec<String>` from literals — recipients in one test, capability names
    /// in the next. Both APIs take owned strings the caller already resolved.
    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// A recipient on the register refuses the act, and the refusal NAMES the
    /// reason and the moment it was established.
    ///
    /// Pins the fail-open this gate exists to close: before it, an opt-out was a
    /// record nothing on the send path read, so the act went out and the model
    /// reported it as sent. It also pins the *content* of the refusal — a
    /// message that said only "suppressed" would invite the model to reach the
    /// same person through a second address.
    #[test]
    fn a_suppressed_recipient_refuses_with_the_reason_that_established_it() {
        let (_tmp, layout) = workspace();
        let register = SuppressionRegister::global(layout.clone());
        let scope = SuppressionScope::new("anonymous", "default");
        register
            .suppress(
                &scope,
                "Blocked@Example.com",
                SuppressionReason::OptOut,
                SuppressionEvidence::new(t(1), "unsubscribe-click-77", "ingest-worker"),
                t(2),
            )
            .expect("the opt-out records");

        let refusal = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &owned(&["blocked@example.com"]),
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("a suppressed recipient is refused");

        assert!(
            refusal.starts_with("NOT SENT — this outward action was refused."),
            "{refusal}"
        );
        assert!(
            refusal.contains("1 of this action's 1 recipient(s)"),
            "{refusal}"
        );
        assert!(refusal.contains("`blocked@example.com`"), "{refusal}");
        assert!(refusal.contains("opt_out"), "{refusal}");
        assert!(
            refusal.contains(&t(1).to_rfc3339()),
            "the refusal must name when the opt-out was established, not when we recorded it: \
             {refusal}"
        );

        // The same register, a recipient nobody suppressed: clear, so the gate
        // is discriminating rather than refusing everything.
        assert_eq!(
            contact_refusal(
                Some(&layout),
                Some("anonymous"),
                Some("default"),
                &owned(&["clear@example.com"]),
                Addressing::RecipientRequired,
                t(3),
            ),
            None
        );
    }

    /// An unreadable register refuses instead of reading as an empty one.
    ///
    /// This is the exact fail-open `jsonl` was written to close, arriving at the
    /// one place where it mails somebody who told us to stop: a corrupt log that
    /// folded to "no entries" would clear every recipient on it.
    #[test]
    fn an_unreadable_register_refuses_rather_than_reading_as_empty() {
        let (_tmp, layout) = workspace();
        let register = SuppressionRegister::global(layout.clone());
        let scope = SuppressionScope::new("anonymous", "default");
        register
            .suppress(
                &scope,
                "blocked@example.com",
                SuppressionReason::Complaint,
                SuppressionEvidence::new(t(1), "complaint-9", "ingest-worker"),
                t(2),
            )
            .expect("the complaint records");

        // Corrupt the entry log the register just wrote. A newline-TERMINATED
        // unparseable line is corruption, not a torn append, so the fold refuses.
        let identities = layout
            .scope_root("anonymous", "default")
            .join("suppression")
            .join("global")
            .join("identities");
        let mut corrupted = 0usize;
        for entry in std::fs::read_dir(&identities).expect("the register wrote an identity log") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
                std::fs::write(&path, b"not json at all\n").expect("corrupt the log");
                corrupted += 1;
            }
        }
        assert_eq!(corrupted, 1, "exactly one identity log should exist");

        let refusal = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &owned(&["blocked@example.com"]),
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("an unreadable register refuses");
        assert!(refusal.contains("could not be read"), "{refusal}");
        assert!(refusal.contains("DO NOT SEND"), "{refusal}");
    }

    /// An outward act with no extractable recipient refuses, and so does one
    /// with no scoped store to check against.
    ///
    /// Both are the same failure wearing different hats: the register was not
    /// consulted for anybody, and "not consulted" reads exactly like "nobody was
    /// suppressed" unless something refuses here.
    #[test]
    fn an_act_with_nobody_to_screen_is_refused_not_passed() {
        let (_tmp, layout) = workspace();

        let no_recipients = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &[],
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("an act with no recipient is refused");
        assert!(
            no_recipients.contains("No recipient could be extracted"),
            "{no_recipients}"
        );

        let no_scope = contact_refusal(
            Some(&layout),
            None,
            Some("default"),
            &owned(&["someone@example.com"]),
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("an act with no scope is refused");
        assert!(no_scope.contains("no scoped store"), "{no_scope}");

        let no_store = contact_refusal(
            None,
            Some("anonymous"),
            Some("default"),
            &owned(&["someone@example.com"]),
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("an act with no artifact workspace is refused");
        assert!(no_store.contains("no scoped store"), "{no_store}");
    }

    /// An act that MAY reach people but need not — a calendar entry with no
    /// attendees — is not refused for having nobody to screen.
    ///
    /// The first cut refused it, which reads as rigour and is really an
    /// over-gate: it would block booking one's own time, the exact act
    /// `scheduling::consumer` performs when a slot is agreed. An empty list
    /// means "we could not tell who this reaches" only for a channel that
    /// exists to reach somebody; on a calendar it means the entry reaches
    /// nobody, and there is nobody to screen. The act is still recorded as a
    /// disclosure — only the screen is skipped.
    #[test]
    fn an_act_that_may_reach_nobody_is_not_refused_for_reaching_nobody() {
        let (_tmp, layout) = workspace();

        assert!(
            contact_refusal(
                Some(&layout),
                Some("anonymous"),
                Some("default"),
                &[],
                Addressing::MayReachNobody,
                t(3),
            )
            .is_none(),
            "an entry on one's own calendar has nobody to screen, not an unreadable audience"
        );

        // The distinction is drawn from the capability's class, never from the
        // arguments — arguments are what we just failed to read.
        assert_eq!(
            Addressing::for_class(OutwardClass::Mail),
            Addressing::RecipientRequired
        );
        assert_eq!(
            Addressing::for_class(OutwardClass::Message),
            Addressing::RecipientRequired
        );
        assert_eq!(
            Addressing::for_class(OutwardClass::CalendarInvite),
            Addressing::MayReachNobody
        );
        assert_eq!(
            Addressing::for_class(OutwardClass::FormSubmission),
            Addressing::MayReachNobody
        );
    }

    /// An empty recipient list refuses only where empty means "we failed to read
    /// who this reaches", and where it does not, the register is never opened.
    ///
    /// Pins the header sentence that had drifted: *"Every refusal below fires on
    /// absence… An empty recipient list… [is] a refusal"* was written before
    /// [`Addressing`] existed and stopped being true for two of the four outward
    /// classes. It matters because the same paragraph is what a reviewer reads
    /// to decide whether the suppression check covers every send; it does not,
    /// and the exception has to be visible rather than discovered.
    ///
    /// The register here is deliberately CORRUPT. A `MayReachNobody` act with no
    /// recipients returns `None` anyway, which is only possible if the register
    /// was never consulted — an unreadable one refuses, as the second half
    /// shows.
    #[test]
    fn an_empty_recipient_list_refuses_only_where_empty_means_unread() {
        let (_tmp, layout) = workspace();
        let register = SuppressionRegister::global(layout.clone());
        let scope = SuppressionScope::new("anonymous", "default");
        register
            .suppress(
                &scope,
                "blocked@example.com",
                SuppressionReason::OptOut,
                SuppressionEvidence::new(t(1), "unsubscribe-2", "ingest-worker"),
                t(2),
            )
            .expect("the opt-out records");
        let identities = layout
            .scope_root("anonymous", "default")
            .join("suppression")
            .join("global")
            .join("identities");
        let mut corrupted = 0usize;
        for entry in std::fs::read_dir(&identities).expect("the register wrote an identity log") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
                std::fs::write(&path, b"not json at all\n").expect("corrupt the log");
                corrupted += 1;
            }
        }
        assert_eq!(corrupted, 1, "exactly one identity log should exist");

        // Empty and MAY reach nobody: no refusal, and the corrupt register was
        // therefore never read.
        assert_eq!(
            contact_refusal(
                Some(&layout),
                Some("anonymous"),
                Some("default"),
                &[],
                Addressing::MayReachNobody,
                t(3),
            ),
            None,
            "an act that may reach nobody must pass the contact gate without opening the register"
        );

        // Empty and MUST name a recipient: refused, and refused for the reading
        // failure rather than for the corruption — the register is still not
        // reached, because there is nobody to look up.
        let unread = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &[],
            Addressing::RecipientRequired,
            t(3),
        )
        .expect("an act whose audience cannot be named is refused");
        assert!(
            unread.contains("No recipient could be extracted"),
            "{unread}"
        );

        // One named recipient on the same class the first assertion passed:
        // now the register IS consulted, and its corruption refuses. Without
        // this the first assertion would also pass for a gate that never
        // screened anybody at all.
        let consulted = contact_refusal(
            Some(&layout),
            Some("anonymous"),
            Some("default"),
            &owned(&["blocked@example.com"]),
            Addressing::MayReachNobody,
            t(3),
        )
        .expect("a named recipient is screened whatever the addressing");
        assert!(consulted.contains("could not be read"), "{consulted}");
        assert!(consulted.contains("DO NOT SEND"), "{consulted}");
    }

    /// Question 3 is recorded on every live send and now asked by exactly one
    /// module.
    ///
    /// Pins the header's third bullet at both ends. `record` is on the live
    /// dispatch path; `unreconciled` is read back by
    /// `delivery_hygiene::silence`, which is what the delivery-hygiene worker's
    /// tick and the owner-facing read both go through.
    ///
    /// The read half spent its whole life at zero callers, which is the state
    /// this test was originally written to pin: the candidate list accumulated
    /// forever and nothing ever looked at it, so a silently broken provider
    /// integration produced no signal at all. **If this list empties again, the
    /// signal is gone** — correct the header rather than the assertion.
    #[test]
    fn the_dispatch_log_is_written_on_every_send_and_read_back_by_the_silence_watch() {
        use crate::magician_v2::doc_wiring_scan::scan_workspace;

        const OWN_FILE: &str = "magician/src/magician_v2/agents/outward_gate.rs";

        // The write half, on the one dispatch path every action passes through.
        let writers = scan_workspace("dispatch_log.record(", &[OWN_FILE]);
        assert!(
            writers.files_searched > 100,
            "only {} files were read, so this proves nothing",
            writers.files_searched
        );
        assert_eq!(
            writers.hits,
            vec!["magician/src/magician_v2/execution/agentic/executor.rs".to_string()],
            "the set of modules recording a dispatch changed; the header names the executor"
        );

        // The read half. This file is excluded because its own tests pair
        // `DispatchLog` against `unreconciled`, and the ledger is excluded
        // because it defines and tests the method.
        let sweepers = scan_workspace(
            ".unreconciled(",
            &[
                OWN_FILE,
                "magician/src/magician_v2/delivery/mod.rs",
                "magician/src/magician_v2/delivery/tests.rs",
            ],
        );
        assert_eq!(
            sweepers.hits,
            vec!["magician/src/magician_v2/delivery_hygiene/silence.rs".to_string()],
            "the set of modules reading the dispatch log back changed; the header names the \
             silence watch"
        );
    }

    /// A dispatched act shows up in `unreconciled()` until a receipt lands, and
    /// disappears the moment one does.
    ///
    /// Pins the whole point of recording the dispatch: without the candidate
    /// list, `unreconciled()` has nothing to sweep and every live send sits at
    /// `dispatch_unknown` forever with nothing able to say so.
    #[test]
    fn a_dispatched_act_stays_unreconciled_until_a_receipt_lands() {
        let (_tmp, layout) = workspace();
        let log = DispatchLog::new(layout.clone());
        let ledger = DeliveryLedger::new(layout.clone());
        let scope = DeliveryScope::new("anonymous", "default");

        log.record(&scope, "act-1", t(1))
            .expect("the dispatch records");

        let overdue = log
            .unreconciled(&ledger, &scope, Duration::hours(1), t(3))
            .expect("the sweep runs");
        assert_eq!(overdue.len(), 1);
        assert_eq!(overdue[0].act_ref, "act-1");
        assert_eq!(overdue[0].dispatched_at, t(1));
        assert_eq!(overdue[0].silent_for, Duration::hours(2));

        // Expiry is INCLUSIVE: silent for exactly the grace period is overdue.
        let exactly = log
            .unreconciled(&ledger, &scope, Duration::hours(2), t(3))
            .expect("the sweep runs");
        assert_eq!(exactly.len(), 1);

        ledger
            .reconcile(
                &scope,
                "act-1",
                &DeliveryReceipt {
                    provider: "agentmail".to_string(),
                    provider_message_id: "pm-1".to_string(),
                    identity: "someone@example.com".to_string(),
                    state: DeliveryState::Accepted,
                    observed_at: t(2),
                    payload_ref: "webhook-1".to_string(),
                },
                t(2),
            )
            .expect("the receipt reconciles");

        let after = log
            .unreconciled(&ledger, &scope, Duration::hours(1), t(3))
            .expect("the sweep runs");
        assert_eq!(
            after.len(),
            0,
            "an acknowledged act is no longer unreconciled, even on a bare acceptance"
        );
    }

    /// Replaying the identical dispatch resumes; the same act ref with a
    /// different dispatch time is an error.
    ///
    /// Pins the two ways a retry could corrupt the silence window: a doubled
    /// record, or a second write that quietly moved the dispatch time forward
    /// and shortened how long the act has been silent.
    #[test]
    fn a_replayed_dispatch_resumes_and_a_changed_one_is_an_error() {
        let (_tmp, layout) = workspace();
        let log = DispatchLog::new(layout.clone());
        let scope = DeliveryScope::new("anonymous", "default");

        log.record(&scope, "act-1", t(1)).expect("first");
        log.record(&scope, "act-1", t(1))
            .expect("identical replay resumes");
        assert_eq!(log.dispatched(&scope).expect("fold").len(), 1);

        let error = log
            .record(&scope, "act-1", t(2))
            .expect_err("a changed dispatch time is an error");
        assert!(error.to_string().contains("reconciled"), "{error}");

        // And the log still holds the first account, unchanged.
        let held = log.dispatched(&scope).expect("fold");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].dispatched_at, t(1));
    }

    /// An act ref carrying the id separator is refused before it is recorded.
    #[test]
    fn an_act_ref_holding_the_field_separator_is_refused() {
        let (_tmp, layout) = workspace();
        let log = DispatchLog::new(layout);
        let scope = DeliveryScope::new("anonymous", "default");
        let error = log
            .record(&scope, "act\u{1f}1", t(1))
            .expect_err("U+001F is refused");
        assert!(error.to_string().contains("U+001F"), "{error}");
    }

    /// A capability outside the work context's effective capabilities refuses,
    /// and each of the four standings gets its own answer.
    ///
    /// Pins the gate that made `work_context` load-bearing instead of inert: an
    /// engagement-bound execution could previously reach for any capability its
    /// agent happened to hold, whatever the work was for.
    #[test]
    fn a_capability_outside_the_work_context_is_refused() {
        let held = owned(&["gmail", "websearch"]);
        let work = WorkContext::new(WorkContextKind::Engagement("eng-1".to_string()))
            .needing(&["websearch", "calendar"]);
        let bound = WorkBinding::Bound(work);

        // Usable: held AND named by the work. The only arm that proceeds.
        assert_eq!(work_context_refusal(&held, &bound, "websearch"), None);

        // OutsideThisWork: held, and this work is not what it is for.
        let outside = work_context_refusal(&held, &bound, "gmail")
            .expect("a capability the work does not name is refused");
        assert!(
            outside.contains("is not part of what `engagement:eng-1` is for"),
            "{outside}"
        );
        assert!(
            outside.contains("Usable inside `engagement:eng-1`: websearch."),
            "{outside}"
        );

        // RequiresDelegation: the work needs it, this agent cannot do it.
        let delegate = work_context_refusal(&held, &bound, "calendar")
            .expect("a capability the agent does not hold is refused");
        assert!(delegate.contains("must be delegated"), "{delegate}");
        assert!(delegate.contains("calendar"), "{delegate}");

        // Unavailable: neither side has anything to do with it.
        let unavailable = work_context_refusal(&held, &bound, "telegram")
            .expect("a capability nobody names is refused");
        assert!(
            unavailable.contains("neither held by this agent nor named by `engagement:eng-1`"),
            "{unavailable}"
        );
    }

    /// An execution bound to a work context that could not be resolved refuses,
    /// while one bound to nothing has no opinion.
    ///
    /// Pins the difference an `Option` would have erased: "there is no work
    /// context" and "there is one and the store could not answer for it" must
    /// not render alike, or an unavailable authority reads as an unrestricted
    /// one.
    #[test]
    fn an_unresolvable_work_context_refuses_but_an_unbound_one_has_no_opinion() {
        let held = owned(&["gmail"]);

        assert_eq!(
            work_context_refusal(&held, &WorkBinding::Unbound, "gmail"),
            None,
            "no work context is no opinion, not a verdict"
        );

        let refusal = work_context_refusal(
            &held,
            &WorkBinding::Unresolvable("Expired".to_string()),
            "gmail",
        )
        .expect("an unresolvable work context refuses");
        assert!(refusal.contains("could not be resolved"), "{refusal}");
        assert!(refusal.contains("Expired"), "{refusal}");
    }

    /// A work context that narrows to nothing says so in words.
    ///
    /// An empty capability list rendered as an empty string reads as a
    /// formatting bug; it is in fact the finding — this agent and this work
    /// share no capability at all.
    #[test]
    fn a_work_context_that_narrows_to_nothing_says_so() {
        let held = owned(&["gmail"]);
        let work =
            WorkContext::new(WorkContextKind::Program("p-1".to_string())).needing(&["calendar"]);
        let refusal =
            work_context_refusal(&held, &WorkBinding::Bound(work), "gmail").expect("refused");
        assert!(
            refusal.contains("<nothing: this agent and this work share no capability>"),
            "{refusal}"
        );
    }
}
